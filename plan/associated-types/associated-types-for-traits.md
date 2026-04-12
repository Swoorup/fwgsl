# Plan: Add Associated Types for Traits

Created: 2026-04-12
Progress: 7/7 phases

## Context

The current trait system uses multi-parameter traits (e.g., `trait Add a b c where (+) : a -> b -> c`). We need to migrate to associated types (e.g., `trait Add a b where type Output; (+) : a -> b -> Output`).

**Why this change:**
- Associated types are more ergonomic when the result type is determined by the operand types
- They better express the relationship between trait parameters and result types
- Common pattern in Rust/Haskell that developers expect

**Intended outcome:**
- `add x y = x + y` infers `Add a b => a -> b -> a.Output`
- Existing concrete impls still require exact match
- No runtime change - associated types are erased during lowering

---

## Phase 1: Parser Changes

### Files to modify
- `/Users/swoorup/github/fwgsl/crates/shadml_parser/src/parser.rs`

### New AST structures

```rust
// Add to TraitDecl
pub struct TraitDecl {
    name: String,
    vars: Vec<String>,
    associated_types: Vec<AssociatedTypeDecl>,  // NEW
    methods: Vec<TraitMethod>,
    // ...
}

pub struct AssociatedTypeDecl {
    pub name: String,  // e.g., "Output"
    pub span: Span,
}

// Add to ImplDecl
pub struct ImplDecl {
    trait_name: Option<String>,
    tys: Vec<Type>,
    associated_types: Vec<AssociatedTypeDef>,  // NEW
    methods: Vec<ImplMethod>,
    // ...
}

pub struct AssociatedTypeDef {
    pub name: String,   // e.g., "Output"
    pub ty: Type,       // e.g., F32
    pub span: Span,
}
```

### Grammar changes

```
# Inside trait declaration (where block):
assoc_type_decl : 'type' UpperIdent

# Inside impl declaration (where block):
assoc_type_def : 'type' UpperIdent '=' type
```

### Implementation steps
1. Add `AssociatedTypeDecl` and `AssociatedTypeDef` structs
2. Modify `TraitDecl` to include `associated_types: Vec<AssociatedTypeDecl>`
3. Modify `ImplDecl` and `BuiltinImplDecl` to include `associated_types: Vec<AssociatedTypeDef>`
4. Add `parse_trait_decl()` parsing for `type Name` lines
5. Add `parse_impl_decl()` parsing for `type Name = Type` lines
6. Update tree-sitter grammar if needed

---

## Phase 2: Type System Changes

### Files to modify
- `/Users/swoorup/github/fwgsl/crates/shadml_typechecker/src/lib.rs`

### New `Ty` variant

```rust
pub enum Ty {
    // ... existing variants ...

    /// Associated type projection: `T.Name` where T is bound by a trait constraint.
    /// E.g., `a.Output` in `Add a b => a -> b -> a.Output`
    AssocProj {
        base: Box<Ty>,      // The base type (e.g., Ty::Var(a))
        name: String,       // The associated type name (e.g., "Output")
        trait_name: String, // The trait that defines this (e.g., "Add")
    },
}
```

### Required updates
1. Add `AssocProj` variant to `Ty` enum
2. Update `apply_subst` - traverse through `AssocProj` base
3. Update `free_vars` - collect free vars from base
4. Update `contains_var` - check base
5. Update `Display` impl - render as `base.name`
6. Update `Ty::concrete()` - treat as concrete if base is concrete

---

## Phase 3: Semantic Analysis Changes

### Files to modify
- `/Users/swoorup/github/fwgsl/crates/shadml_semantic/src/lib.rs`

### Update `TraitInfo`

```rust
pub struct TraitInfo {
    pub name: String,
    pub vars: Vec<String>,
    pub var_ids: Vec<TyVarId>,
    pub associated_types: Vec<String>,  // NEW: names like ["Output"]
    pub methods: Vec<(String, Ty)>,
}
```

### Update `ImplInfo` and `BuiltinImplInfo`

```rust
pub struct ImplInfo {
    pub trait_name: Option<String>,
    pub tys: Vec<Ty>,
    pub associated_type_bindings: HashMap<String, Ty>,  // NEW: "Output" -> F32
    pub methods: HashMap<String, String>,
}

pub struct BuiltinImplInfo {
    pub trait_name: String,
    pub tys: Vec<Ty>,
    pub associated_type_bindings: HashMap<String, Ty>,  // NEW
    pub methods: HashMap<String, BuiltinLowering>,
}
```

### Implementation steps
1. Add `associated_types: Vec<String>` to `TraitInfo`
2. Add `associated_type_bindings: HashMap<String, Ty>` to `ImplInfo` and `BuiltinImplInfo`
3. Update trait declaration analysis (Pass 2b):
   - Collect associated type names from AST
   - When converting method types, recognize associated type names as projections
4. Update impl declaration analysis (Pass 2c):
   - Collect associated type definitions
   - Validate all trait's associated types are defined
   - Substitute associated type projections with concrete types in method signatures
5. Update `replace_trait_vars()` to handle `AssocProj`

---

## Phase 4: Inference and Constraint Solving

### Files to modify
- `/Users/swoorup/github/fwgsl/crates/shadml_semantic/src/lib.rs`

### Key changes

1. **Associated type projection in method types:**
   - Method signatures like `(+) : a -> b -> Output` where `Output` is an associated type
   - Convert to `Ty::AssocProj { base: Ty::Var(a), name: "Output", trait_name: "Add" }`

2. **During predicate improvement (`try_improve_predicate_with_impls`):**
   - When impl matches, also unify associated type projections
   - If predicate has `Add a b => a.Output`, and impl has `Add F32 F32 where Output = F32`
   - Then `a.Output` unifies with `F32`

3. **Normalization:**
   - Add `normalize_assoc_type()` function to resolve projections when base is concrete

---

## Phase 5: Lowering Changes

### Files to modify
- `/Users/swoorup/github/fwgsl/crates/shadml_ast_lowering/src/lib.rs`

### Key insight
Associated types are erased during lowering. Name mangling uses resolved concrete types.

### Implementation steps
1. When resolving trait methods, look up associated type bindings from matching impl
2. Substitute `AssocProj` types with concrete types before computing type suffix
3. The mangling `methodName_Type1__Type2` remains unchanged - types are now resolved

---

## Phase 6: Prelude Migration

### Files to modify
- `/Users/swoorup/github/fwgsl/prelude/prelude.shadml`

### Migration pattern

**Before (multi-parameter):**
```
trait Add a b c where
  (+) : a -> b -> c

builtin impl Add F32 F32 F32 where
  (+) = native_binop (+)
```

**After (associated type):**
```
trait Add a b where
  type Output
  (+) : a -> b -> Output

builtin impl Add F32 F32 where
  type Output = F32
  (+) = native_binop (+)
```

### Traits to migrate
- `Add a b c` → `Add a b` with `type Output`
- `Sub a b c` → `Sub a b` with `type Output`
- `Mul a b c` → `Mul a b` with `type Output`
- `Div a b c` → `Div a b` with `type Output`
- `Mod a b c` → `Mod a b` with `type Output`
- `BitAnd a b c` → `BitAnd a b` with `type Output`
- `BitXor a b c` → `BitXor a b` with `type Output`
- `Shl a b c` → `Shl a b` with `type Output`
- `Shr a b c` → `Shr a b` with `type Output`

**Note:** `Neg a` and `BitNot a` are single-parameter and don't need associated types.

---

## Verification

### Test cases
1. **Parser:** Parse `trait Add a b where type Output; (+) : a -> b -> Output`
2. **Parser:** Parse `impl Add F32 F32 where type Output = F32; (+) x y = x + y`
3. **Type inference:** `add x y = x + y` should infer `Add a b => a -> b -> a.Output`
4. **Lowering:** Verify mangled function names include resolved types
5. **Prelude:** All existing operator tests continue to pass

### How to test
```bash
# Run parser tests
cargo test -p shadml_parser

# Run typechecker tests
cargo test -p shadml_typechecker

# Run integration tests
cargo test -p shadml_integration_tests

# Build examples
cargo build --example traits
```

---

## Implementation Order

1. **Parser AST nodes** - Add structs, non-breaking
2. **Parser parsing** - Parse `type Name` in traits, `type Name = Type` in impls
3. **Type `AssocProj`** - Add variant to `Ty` enum
4. **Semantic `TraitInfo`/`ImplInfo`** - Add associated type fields
5. **Semantic analysis** - Collect and validate associated types
6. **Type inference** - Handle `AssocProj` in substitution and predicate improvement
7. **Lowering** - Resolve projections to concrete types
8. **Prelude migration** - Update operator traits
9. **Tests** - Add comprehensive test coverage

---

## Critical Files

| File | Changes |
|------|---------|
| `crates/shadml_parser/src/parser.rs` | AST nodes, parsing logic |
| `crates/shadml_typechecker/src/lib.rs` | `Ty::AssocProj` variant |
| `crates/shadml_semantic/src/lib.rs` | `TraitInfo`, `ImplInfo`, predicate improvement |
| `crates/shadml_ast_lowering/src/lib.rs` | Associated type resolution |
| `prelude/prelude.shadml` | Operator trait migration |