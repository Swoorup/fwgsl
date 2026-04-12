# Multi-Parameter Operator Traits Plan

## Goal

Provide a coherent language feature for heterogeneous operators before adding
or changing builtin/backend behavior. The immediate motivating case is
vector-scalar arithmetic such as:

```shadml
v * s
s * v
```

but the design should also cover matrix-scalar, matrix-vector, and future
numeric abstractions without introducing one-off typing rules.

## Non-Goals

- Do not add parser or semantic magic that special-cases `Vec * scalar`.
- Do not encode heterogeneous arithmetic purely as backend WGSL knowledge.
- Do not introduce overlapping impls, specialization, or unrestricted Haskell
  style typeclass machinery in the first iteration.

## Current State

The current implementation has three relevant constraints:

1. Operator traits are unary and homogeneous in the spec and implementation.
   Example: `trait Mul a where (*) : a -> a -> a`.
2. Trait metadata and dispatch are receiver-oriented.
3. Infix typing currently assumes homogeneous builtins and then lowers plain
   binary operators directly to MIR/WGSL.

This means the language cannot currently express `Vec<n, a> * a -> Vec<n, a>`
as a principled trait-level relation.

## Language Design

### 1. Generalize operator traits

Move arithmetic and bitwise binary operator traits from:

```shadml
trait Mul a where (*) : a -> a -> a
```

to:

```shadml
trait Mul a b c where
  (*) : a -> b -> c
```

and similarly for:

- `Add`
- `Sub`
- `Div`
- `Mod`
- `BitAnd`
- `BitXor`
- `Shl`
- `Shr`

Unary traits remain unary:

```shadml
trait Neg a where negate : a -> a
trait BitNot a where bitnot : a -> a
```

### 2. Dot syntax remains sugar

Preserve:

```shadml
x.method y  =>  method x y
```

Resolution must happen through normal constraint solving on the desugared call,
not a separate receiver-only path.

### 3. Conservative trait system scope

First implementation should support:

- multi-parameter traits
- exact concrete impl-head matching
- explicit constraints in type signatures
- no overlapping impls
- no specialization
- no functional dependencies
- no associated types

This keeps the model compatible with the compiler's current preference for
static, exact-match impl resolution.

### 4. Constrained inference and signatures

Operator use must participate in ordinary HM-style inference. For example:

```shadml
add x y = x + y
```

should infer a type in the shape:

```shadml
add : Add a b c => a -> b -> c
```

That inferred predicate is part of the language model; the compiler should not
pretend that operators are special builtins outside constraint generation.

However, shadml should also keep module interfaces explicit and diagnostics
predictable. The intended rule for the first implementation is:

- expression typing may infer predicates such as `Add a b c`
- if those predicates are discharged to concrete impls during solving, no
  explicit signature is required
- if a top-level binding generalizes to a constrained type, the binding must
  carry an explicit type signature spelling out those constraints
- local bindings may retain inferred constrained types without an explicit
  signature in the first iteration

This gives the compiler principled overloaded-function inference while keeping
public APIs explicit.

## Builtin / Prelude Model

After the language feature exists, represent builtin arithmetic through
predeclared traits plus builtin impl inventory.

Examples of desired builtin relations:

```shadml
Mul F32 F32 F32
Mul I32 I32 I32
Mul U32 U32 U32

Mul (Vec n F32) F32 (Vec n F32)
Mul F32 (Vec n F32) (Vec n F32)
Mul (Vec n F32) (Vec n F32) (Vec n F32)

Mul (Mat r c F32) F32 (Mat r c F32)
Mul F32 (Mat r c F32) (Mat r c F32)
Mul (Mat r c F32) (Vec c F32) (Vec r F32)
```

The compiler may still lower these to native WGSL operators, constructors, or
intrinsics, but only after the type system has expressed the relation.

## Compiler Work Plan

### Phase 1: Spec and AST shape

1. Update `spec.md` to describe current homogeneous traits as transitional.
2. Extend parser AST for trait declarations and type constraints so trait heads
   can bind multiple type variables.
3. Extend impl syntax representation to store full trait-head argument lists.

### Phase 2: Type representation and predicates

1. Replace single-type predicates of the shape:

```rust
Predicate { trait_name, ty }
```

with a shape closer to:

```rust
Predicate { trait_name, tys: Vec<Ty> }
```

2. Update generalization, instantiation, substitution, and pretty-printing for
   multi-argument predicates.
3. Update parser conversion from syntax constraints like:

```shadml
Mul a b c => ...
```

into semantic predicates.
4. Ensure generalized schemes can retain inferred multi-parameter predicates,
   even when a later top-level validation pass requires those predicates to be
   written explicitly in source.

### Phase 3: Trait and impl collection

1. Generalize `TraitInfo` so it stores all trait parameters, not one receiver
   variable.
2. Generalize impl metadata so a trait impl head stores the full type list.
3. Preserve the current exact-match rule, but compare the full impl head.
4. Keep rejection of overlapping impls by exact concrete-head conflict checks.

### Phase 4: Expression typing

1. Infix operators should no longer be assumed to have type `a -> a -> a`.
2. Instead, operator lookup should:
   - introduce fresh operand/result variables
   - emit the appropriate predicate, e.g. `Mul lhs rhs out`
   - return `out`
3. Normal function application of trait methods must likewise instantiate the
   full trait method scheme and emit matching constraints.
4. Dot syntax should remain a surface rewrite only.

### Phase 5: Constraint solving

1. Extend trait resolution so it solves predicates over full heads rather than
   receiver type only.
2. First iteration can stay simple:
   - fully resolve operand/result types
   - look for an exact builtin or user impl match
   - otherwise emit a missing impl / missing constraint diagnostic
3. Defer sophisticated improvement/defaulting rules unless they are required by
   real examples.
4. After inference, distinguish between:
   - predicates solved away by concrete types
   - predicates retained in the generalized scheme
5. If a top-level binding retains predicates in its generalized scheme and has
   no explicit signature, emit a diagnostic asking for that constrained type
   signature to be written out.

### Phase 6: Lowering

1. Once a trait-backed operator resolves to a builtin arithmetic relation,
   lower it either to:
   - a native `BinOp` when WGSL supports the exact relation, or
   - a builtin helper call if a constructor/intrinsic expansion is required.
2. User-defined trait impls should continue lowering to mangled functions.
3. Keep MIR and WGSL codegen simple; the semantic layer should decide whether a
   resolved operator is native or user-defined.

### Phase 7: Tooling and tests

Add tests for:

- homogeneous scalar arithmetic still working
- user-defined same-type operator impls still working
- vector-scalar and scalar-vector multiplication
- matrix-vector multiplication
- missing constraint diagnostics
- inferred operator constraints on generic bindings
- missing explicit top-level signatures for constrained bindings
- explicit top-level constrained signatures being accepted
- local constrained bindings being inferred without requiring signatures
- conflicting impl-head diagnostics
- dot syntax with multi-parameter method constraints

## Migration Strategy

1. Land the trait/predicate generalization first.
2. Preserve old homogeneous operator traits temporarily through compatibility
   handling if needed during the transition.
3. Migrate the prelude/spec to multi-parameter operator classes.
4. Add builtin impl inventory for WGSL-backed numeric relations.
5. Then simplify any transitional native-operator shortcuts that duplicate the
   new model.

## Risks

### Constraint ambiguity

Partial application and generic helper functions may produce unsolved
multi-parameter constraints more often than the current unary model.

Mitigation:

- infer predicates normally, then require explicit top-level signatures when
  constrained schemes escape into module scope
- keep diagnostics explicit
- avoid adding advanced inference tricks too early

### Implementation spread

This change touches parser, semantic analysis, trait storage, inference,
lowering, diagnostics, and tests.

Mitigation:

- land in phases
- keep MIR/codegen changes minimal
- add characterization tests before broad refactors

### Over-design

It is easy to drift into a near-Haskell typeclass system.

Mitigation:

- keep exact-match impl resolution
- no overlap
- no fundeps
- no associated types
- no specialization in the first pass

## Acceptance Criteria

The feature is ready when:

1. The language can express heterogeneous operator relations via traits.
2. `Vec * scalar` and `scalar * Vec` typecheck through trait constraints, not
   ad hoc compiler rules.
3. Dot syntax continues to work as ordinary receiver-first sugar.
4. Builtin WGSL-backed arithmetic is represented as builtin impls or equivalent
   predeclared trait relations.
5. Operator expressions such as `x + y` infer trait predicates as part of
   ordinary type inference.
6. Top-level constrained bindings require explicit type signatures, while local
   constrained bindings may remain inferred.
7. The spec and implementation describe the same model.
