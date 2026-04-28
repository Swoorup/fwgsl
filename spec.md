# shadml Language Specification

**Version:** 0.2.0
**Status:** Working draft

shadml is a purely functional language that compiles to WGSL (WebGPU Shading Language). It provides Haskell-inspired syntax with indentation-sensitive layout, Hindley-Milner type inference, algebraic data types, pattern matching, and traits — all targeting the GPU via WGSL code generation.

---

## Table of Contents

1. [Lexical Structure](#1-lexical-structure)
2. [Layout Rules](#2-layout-rules)
3. [Types](#3-types)
4. [Declarations](#4-declarations)
5. [Expressions](#5-expressions)
6. [Patterns](#6-patterns)
7. [Operators](#7-operators)
8. [Type System](#8-type-system)
9. [Traits and Implementations](#9-traits-and-implementations)
10. [Module System](#10-module-system)
11. [Conditional Compilation](#11-conditional-compilation)
12. [Bitfields](#12-bitfields)
13. [Entry Points and GPU Resources](#13-entry-points-and-gpu-resources)
14. [Prelude and Builtins](#14-prelude-and-builtins)
15. [Compilation Pipeline](#15-compilation-pipeline)
16. [WGSL Code Generation](#16-wgsl-code-generation)
17. [Tooling](#17-tooling)

---

## 1. Lexical Structure

### 1.1 Comments

```
-- Line comment (to end of line)
{- Block comment (may be nested) -}
```

### 1.2 Identifiers

| Form | Description | Examples |
|------|-------------|---------|
| `Ident` | Lower-case or underscore-leading | `foo`, `_tmp`, `myVar` |
| `UpperIdent` | Starts with uppercase letter | `Vec`, `MyStruct`, `Some` |
| `Operator` | Symbolic operator in parentheses | `(+)`, `(==)`, `(%)` |

### 1.3 Keywords

```
module  where   import  data    alias   extern  uniform  storage
trait   impl    let     in      case    of      match
if      then    else    do      forall  infixl  infixr
infix   deriving  bitfield  const  loop  as  when  cfg
builtin  type  Self  immediate
```

### 1.4 Integer Literals

| Form | Description | Examples |
|------|-------------|---------|
| Decimal | Unsuffixed → I32 | `42`, `0`, `1000` |
| Decimal + `u` | U32 suffix | `42u`, `0u` |
| Decimal + `i` | Explicit I32 suffix | `42i` |
| Hexadecimal | `0x` or `0X` prefix | `0xFF`, `0xFFu` |
| Octal | `0o` or `0O` prefix | `0o77`, `0o10u` |
| Binary | `0b` or `0B` prefix | `0b1010`, `0b1010u` |

The `u`/`i` suffix is only recognized when **not** followed by an identifier-continue character.

### 1.5 Float Literals

```
3.14    0.5    1.0e10    2.5E-3    0.0
```

Floats always contain a decimal point or exponent. There is no `f` suffix.

### 1.6 Negative Literal Atoms

A minus sign (`-`) immediately followed by a digit (no intervening whitespace) is parsed as a **negative literal atom** in function application position. This allows:

```
vec2 -0.35 0.5    -- parsed as vec2(-0.35, 0.5)
```

When there is whitespace before the `-`, it is parsed as the binary subtraction operator:

```
x - 0.5           -- subtraction
```

### 1.7 String and Character Literals

String literals are delimited by `"..."` and character literals by `'...'`. These are parsed by the lexer but have limited use in the current WGSL target (WGSL has no string type).

### 1.8 Punctuation and Operators

| Token | Symbol | Token | Symbol |
|-------|--------|-------|--------|
| `(` `)` | Parentheses | `[` `]` | Brackets |
| `{` `}` | Braces | `,` | Comma |
| `;` | Semicolon | `:` | Colon |
| `::` | Double colon | `.` | Dot |
| `..` | Range | `->` | Arrow |
| `=>` | Fat arrow | `\` | Backslash (lambda) |
| `@` | Attribute prefix | `\|` | Pipe (match arms) |
| `=` | Equals | `_` | Wildcard |
| `\|>` | Pipeline forward | `$` | Low-precedence apply |
| `` ` `` | Backtick (infix) | `!` | Boolean not |
| `<-` | Left arrow | | |

**Arithmetic:** `+`, `-`, `*`, `/`, `%`

**Comparison:** `==`, `/=` (not equal), `<`, `>`, `<=`, `>=`

**Logical:** `&&`, `||`

**Bitwise:** `&` (AND), `^` (XOR), `~` (NOT prefix), `<<` (shift left). Bitwise OR uses the `bor` function; shift right uses `>>` (two adjacent `>`) or the `shr` function.

---

## 2. Layout Rules

shadml uses **Haskell 2010-style indentation-sensitive layout**. The layout resolver inserts virtual tokens into the token stream:

- `LayoutBraceOpen` — opens an indentation context
- `LayoutSemicolon` — separates declarations/bindings at the same indentation level
- `LayoutBraceClose` — closes an indentation context

### 2.1 Layout Keywords

After `where`, `let`, `of`, and `do`, if the next non-trivia token is not an explicit `{`, the resolver inserts `LayoutBraceOpen` at that token's column and pushes the column onto the indent stack.

### 2.2 Indentation Rules

On each newline, the resolver compares the next token's column to the top of the indent stack:

- **Equal column** → insert `LayoutSemicolon` (new declaration/binding)
- **Lesser column** → pop the stack and insert `LayoutBraceClose`, repeat
- **Greater column** → continuation of the current declaration

### 2.3 Explicit Braces

Within explicit `{ }` braces (records, bitfield declarations), virtual layout tokens are suppressed. The resolver tracks `explicit_brace_depth` and only generates layout tokens at depth 0.

---

## 3. Types

### 3.1 Primitive Types

| shadml Type | WGSL Type | Description |
|------------|-----------|-------------|
| `I32` | `i32` | Signed 32-bit integer |
| `U32` | `u32` | Unsigned 32-bit integer |
| `F32` | `f32` | 32-bit floating point |
| `Bool` | `bool` | Boolean |
| `()` | (void) | Unit type |

### 3.2 Vector Types

```
Vec<N, T>       -- e.g. Vec<2, F32> → vec2<f32>
```

`N` is a natural number literal (2, 3, or 4). `T` is a scalar type.

Aliases: `Vector` is accepted as a synonym for `Vec`.

### 3.3 Matrix Types

```
Mat<R, C, T>    -- e.g. Mat<4, 4, F32> → mat4x4<f32>
```

Alias: `Matrix` is accepted as a synonym for `Mat`.

### 3.4 Array Types

```
Array<T, N>     -- Fixed-size array: array<T, N>
Array<T>        -- Runtime-sized (unsized) array: array<T>
```

Surface `Array<T, N>` is normalized internally to `Tensor T N` (element type first, dimension second).

Aliases: `Tensor` and `Ten` are accepted as synonyms for `Array`.

### 3.5 Function Types

```
A -> B          -- Function from A to B
A -> B -> C     -- Curried: A -> (B -> C)
```

Arrow types are right-associative.

### 3.6 Tuple Types

```
(A, B)          -- Pair
(A, B, C)       -- Triple
```

Tuple types exist in the surface language and type system but are **desugared before codegen**. WGSL has no tuple type.

Tuple-argument functions are **distinct** from curried functions:

```
f : (A, B) -> R
f (a, b) = ...
```

This is not the same type as a curried function:

```
g : A -> B -> R
```

So:

```
f (x, y)            -- one tuple argument
g x y               -- two curried arguments
```

The compiler may still flatten tuple-shaped parameters during backend lowering when it is only an ABI/codegen detail, but tuple-argument and curried functions remain distinct in typing and source checking.

### 3.7 Type Application

```
Maybe I32           -- Type constructor applied to argument
Vec 3 F32           -- Multi-argument application (curried)
```

### 3.8 Type Aliases

```
alias Vec4f = Vec<4, F32>
alias MyArray = Array<F32, 10>
```

Type aliases are expanded during semantic analysis. The `alias` keyword is the only way to declare type aliases (`newtype` is not supported). The `type` keyword is reserved for associated type declarations within trait and impl bodies.
Capitalized names in type position are not implicit aliases: if you want a shorthand like `Vec3F`, write `alias Vec3F = Vec<3, F32>` explicitly.

### 3.9 Type Variables

Lowercase identifiers in type positions are type variables, used for polymorphism:

```
extern sin : a -> a             -- polymorphic
identity : a -> a
identity x = x
```

### 3.10 Binding Address Spaces

Binding declarations use an address space keyword to specify the storage class:

| Address Space | WGSL | Description |
|--------------|------|-------------|
| `uniform` | `var<uniform>` | Uniform buffer (read-only) |
| `storage` | `var<storage>` | Storage buffer (read-only, default) |
| `storage(read)` | `var<storage, read>` | Storage buffer (explicit read-only) |
| `storage(read_write)` | `var<storage, read_write>` | Storage buffer (read-write) |
| `immediate` | `var<immediate>` | Push constants (small read-only data passed per-draw) |
| *(bare name)* | `var` | Opaque handle (textures, samplers, binding arrays) |

When a `@group` / `@binding` declaration omits all address space keywords, the binding is an **opaque handle** (`BindingAddressSpace::Opaque`). This is the correct form for textures, samplers, and binding arrays — WGSL resources that are not buffers:

```
@group(0) @binding(0) myTexture  : Texture2d F32        -- opaque
@group(0) @binding(1) mySampler : Sampler              -- opaque
@group(0) @binding(2) uniform   frameData : FrameData  -- uniform buffer
@group(0) @binding(3) immediate params : PushConstants  -- push constants
```

The inner type `T` is stored directly — no wrapper types are needed.

### 3.11 Texture Types

```
Texture2d F32            -- 2D texture → texture_2d<f32>
Texture2dMultisampled F32 -- 2D multisampled texture → texture_2d<f32>
Texture2dArray F32       -- 2D texture array → texture_2d_array<f32, num>
```

Texture types are declared as `builtin type` in the prelude. They take a single type parameter (the sample type, always `F32` in current WGSL). `Texture2dArray` additionally carries an array size via type application.

### 3.12 Sampler Types

```
Sampler              -- sampler → sampler
SamplerComparison    -- comparison sampler → sampler_comparison
```

Sampler types are declared as `builtin type` in the prelude. They take no type parameters.

### 3.13 Binding Array Type

```
BindingArray T N     -- e.g. BindingArray (Texture2d F32) 8 → binding_array<texture_2d<f32>, 8>
```

Binding arrays group multiple resources of the same type under a single binding slot. They are declared as `builtin type` in the prelude with arity 2. `T` is the element type (typically a texture or sampler) and `N` is the array size.

### 3.14 Dependent Dimensions (Nat)

Natural number literals in type position (`2`, `3`, `4`) are `Ty::Nat` values used for vector/matrix/array dimensions:

```
Vec<3, F32>     -- Nat(3) applied to Vec
```

---

## 4. Declarations

### 4.1 Type Signatures

```
name : Type
name : TraitName a => Type
```

Type signatures are optional but recommended. They precede the corresponding function definition:

```
add : I32 -> I32 -> I32
add x y = x + y
```

Trait-constrained generic functions use `=>` before the main type:

```
lighting : Light a => a -> Vec<3, F32> -> Vec<3, F32>
```

The constraint head may contain multiple type arguments:

```
scale : Mul (Vec 3 F32) F32 => Vec 3 F32 -> F32 -> (Mul (Vec 3 F32) F32).Output
```

Trait-origin calls participate in ordinary type inference. If a top-level
binding generalizes to a constrained type, the binding must carry an explicit
type signature spelling out those constraints. Local `let` / `where` bindings
may retain inferred constrained types without an explicit signature.

### 4.2 Function Declarations

```
name params = body
```

Functions are defined with pattern parameters on the left of `=`:

```
length v = sqrt (dot v v)
```

#### 4.2.1 Guard Clauses

Functions can use guard clauses instead of a single body:

```
abs x
  | x < 0    = 0 - x
  | otherwise = x
```

Guards are delimited by `|` and tested top-to-bottom. `otherwise` is a conventional name (it must be a truthy value).

#### 4.2.2 Where Clauses

Local bindings can be introduced with `where`:

```
circleArea r = pi * r * r
  where pi = 3.14159
```

### 4.3 Data Type Declarations

The `data` keyword declares algebraic data types. It supports several forms:

#### 4.3.1 Records (Structs)

```
data Point = Point {
  x : F32,
  y : F32,
}
```

Record fields may carry attributes:

```
data VertexOutput = VertexOutput {
  @builtin(position) clip_position : Vec<4, F32>,
  @location(0)       color         : Vec<4, F32>,
}
```

#### 4.3.2 Pure Enums

```
data Color = Red | Green | Blue
```

Pure enums (constructors with no fields) compile to `u32` values in WGSL.

#### 4.3.3 Explicit Discriminant Values

```
data CapType = NoCap = 0 | Arrow = 1 | Round = 2 | Butt = 3
```

If discriminants are omitted, constructors are assigned sequential indices starting from 0.

#### 4.3.4 Sum Types (ADTs)

```
data Shape = Circle F32 | Rect F32 F32
```

Sum types compile to structs with a `tag: u32` field plus fields for each constructor's payload.

#### 4.3.5 Wrapper Types

```
data Meters = Meters F32
```

#### 4.3.6 Parameterized Types

```
data Option a = Some a | None
data Pair a b = Pair a b
```

### 4.4 Type Alias Declarations

```
alias Name = Type
alias Name params = Type
```

Examples:

```
alias Vec4f = Vec<4, F32>
alias Pos = Vec<3, F32>
```

### 4.5 Constant Declarations

```
const NAME : TYPE = EXPR
```

Constants are module-level immutable bindings that compile to WGSL `const` declarations:

```
const MAX_LIGHTS : I32 = 16
const PI : F32 = 3.14159
```

The name may be in any case (camelCase, SCREAMING_SNAKE_CASE, etc.).

### 4.6 Bitfield Declarations

See [Section 12: Bitfields](#12-bitfields).

### 4.7 Extern Declarations

```
extern name : Type
```

Declares a built-in name with a type signature but no body. Used in the prelude for WGSL builtins:

```
extern sin : a -> a
extern vec2 : a -> a -> Vec<2, a>
```

### 4.8 Builtin Declarations

The `builtin` keyword introduces compiler-internal declarations that are not user-definable but are needed for the prelude to wire up WGSL primitives.

#### 4.8.1 Builtin Type Declarations

```
builtin type Name Arity
```

Declares a compiler-recognized type constructor with its arity (number of type parameters). These types are not `data` declarations — they have no constructors in the shadml sense. Instead, they are opaque types that map directly to WGSL built-in types:

```
builtin type Vec 2
builtin type Texture2d 1
builtin type Sampler 0
builtin type BindingArray 2
```

The prelude uses this mechanism to register `Vec`, `Mat`, `Texture2d`, `Texture2dMultisampled`, `Texture2dArray`, `Sampler`, `SamplerComparison`, `BindingArray`, `Scalar`, and others.

#### 4.8.2 Builtin Extern Declarations

```
builtin extern name : Type = lowering
```

Declares a built-in extern with a lowering specification that tells the compiler how to emit the function in WGSL. The `lowering` specification has three forms:

| Form | Description | Example |
|------|-------------|---------|
| `native_binop (+)` | Binary operator in WGSL | `builtin extern (+) : ... = native_binop (+)` |
| `native_unary (-)` | Unary operator in WGSL | `builtin extern negate : ... = native_unary (-)` |
| `intrinsic sin` | WGSL builtin function | `builtin extern sin : ... = intrinsic sin` |

`native_binop` and `native_unary` lower to WGSL operator syntax. `intrinsic` lowers to a WGSL builtin function call.

#### 4.8.3 Builtin Impl Declarations

```
builtin impl TraitName T1 ... Tn where
  type AssocType = ConcreteType;
  methodName = lowering
```

Declares a built-in trait implementation with associated type definitions and lowering specifications. Used in the prelude to wire primitive types to operator traits:

```
builtin impl Add F32 F32 where
  type Output = F32;
  (+) = native_binop (+)

builtin impl Neg F32 where
  type Output = F32;
  negate = native_unary (-)
```

The `lowering` specifications use the same forms as `builtin extern`: `native_binop`, `native_unary`, and `intrinsic`.

### 4.9 Binding Declarations

```
@group(G) @binding(B) uniform name : T
@group(G) @binding(B) storage name : T
@group(G) @binding(B) storage(read) name : T
@group(G) @binding(B) storage(read_write) name : T
@group(G) @binding(B) immediate name : T
@group(G) @binding(B) name : T
```

Declares a GPU resource binding. The address space is specified by `uniform`, `storage`, or `immediate` keywords.
Bare `storage` defaults to read-only access (consistent with WGSL). Use `storage(read_write)` for read-write access.
Use `immediate` for push constants — small read-only data passed per-draw/dispatch call.
When no address space keyword is present, the binding is an **opaque handle** (used for textures, samplers, and binding arrays).

```
@group(0) @binding(0) uniform             frame     : FrameData
@group(1) @binding(0) storage(read_write) particles : Array<Particle>
@group(2) @binding(0)                     myTexture : Texture2d F32
@group(0) @binding(0) immediate           params    : PushConstants
```

#### Group Block Sugar

When multiple bindings share a group, the group can be specified once with indented bindings:

```
@group(0)
  @binding(0) uniform             frame     : FrameData
  @binding(1) uniform             params    : DrawParams
  @binding(2) storage(read_write) output    : Array<Vec<4, F32>, 64>
@group(1)
  @binding(0) myTexture  : Texture2d F32
  @binding(1) mySampler  : Sampler
@group(0)
  @binding(3) immediate params : PushConstants
```

This is equivalent to writing the `@group` on each line. The indented bindings inherit the group number.

### 4.10 Trait Declarations

See [Section 9: Traits](#9-traits-and-implementations).

### 4.11 Impl Declarations

See [Section 9: Traits](#9-traits-and-implementations).

### 4.12 Entry Point Declarations

Entry points are function declarations preceded by stage attributes:

```
main : ComputeInput -> ()
@compute @workgroup_size(64, 1, 1)
main input = ...
```

Supported stages:

| Attribute | WGSL Stage |
|-----------|------------|
| `@compute` | `@compute` |
| `@vertex` | `@vertex` |
| `@fragment` | `@fragment` |

Additional attributes: `@workgroup_size(x, y, z)`

A type signature is required before the entry point definition.

### 4.13 Module Declarations

```
module Name.Path
```

Optional header. If absent, the module name is derived from the file path. See [Section 10: Module System](#10-module-system).

### 4.14 Import Declarations

```
import Foo
import Foo (bar, baz)
import Foo as F
import Foo.*
import Debug when cfg.debug
```

See [Section 10: Module System](#10-module-system).

---

## 5. Expressions

### 5.1 Literals

```
42              -- I32 integer
42u             -- U32 integer
42i             -- I32 (explicit suffix)
0xFF            -- Hex I32
0xFFu           -- Hex U32
3.14            -- F32 float
true / false    -- Bool (constructors, not keywords)
```

### 5.2 Variables and Constructors

```
foo             -- Variable (lowercase)
MyConstructor   -- Constructor (uppercase)
```

### 5.3 Function Application

```
f x             -- Apply f to x
f x y           -- Curried: (f x) y
f (g x)         -- Nested application
f [1.0, 0.0]    -- Vector literal as an argument
```

Application is left-associative and binds tighter than all infix operators (binding power 11).

Square brackets are parsed as indexing only when they are immediately attached to
the expression on the left:

```
arr[i]          -- Indexing
f [1, 2, 3]     -- Application of a vector literal
f x [1, 2, 3]   -- ((f x) [1, 2, 3])
```

### 5.4 Binary Operators

```
x + y
a == b
p && q
```

See [Section 7: Operators](#7-operators) for the full precedence table.

### 5.5 Unary Operators

```
-x              -- Arithmetic negation
!b              -- Boolean not
```

Both unary operators have binding power 13 (same as field access and indexing).

### 5.6 Pipeline Operator

```
x |> f          -- Desugars to: f x
x |> f |> g     -- Desugars to: g (f x)
```

The pipeline operator `|>` has the lowest precedence (binding power 1) and is left-associative.

### 5.7 Lambda Expressions

```
\x -> x + 1
\x y -> x + y
```

Lambdas are introduced with `\` (backslash) and use `->` to separate parameters from the body.

When applied immediately, lambdas are **beta-reduced** at compile time:

```
(\x -> x + 1) 5    -- becomes: let x = 5 in x + 1
```

There is no lambda variant in HIR — all lambdas are resolved at application sites.

### 5.8 Let Expressions

```
let x = 1
    y = 2
in x + y
```

Multiple bindings are separated by layout (same indentation level). The scope of each binding extends to all subsequent bindings and the body.

### 5.9 If-Then-Else

```
if cond then trueBranch else falseBranch
```

Both branches are required (expressions, not statements). Multi-line:

```
if x > 0
then x
else -x
```

### 5.10 Match Expressions

```
match expr
  | pattern1 -> body1
  | pattern2 -> body2
  | _        -> default
```

#### 5.10.1 When-Guards

Match arms support optional boolean guards:

```
match expr
  | pattern when condition -> body
  | _                      -> default
```

The guard is evaluated after the pattern matches. If the guard fails, execution falls through to the next arm.

```
classify x = match x
  | _ when x < 0.0  -> 0
  | _ when x < 1.0  -> 1
  | _                -> 2
```

Guards can be combined with any pattern kind (wildcard, variable, constructor, literal):

```
match shape
  | Circle r when r > 10.0 -> "big circle"
  | Circle r               -> "small circle"
  | _                      -> "not a circle"
```

### 5.11 Loop Expressions

Named tail-recursive loops (Scheme-style named `let`):

```
loop name (var1 = init1) (var2 = init2) in body
```

Inside the body, calling `name` with new values performs a tail-recursive jump:

```
sumTo n = loop go (acc = 0) (i = 0) in
  if i < n then go (acc + i) (i + 1) else acc
```

Compiles to WGSL `loop { ... break; }` with `var` bindings and `continue`.

### 5.12 Fold Over Range

```
foldRange start end init (\acc i -> body)
```

Folds a function over an integer range `[start, end)`, threading an accumulator. This is a compiler-recognized function (declared in the prelude) that desugars to an efficient WGSL `for`-style loop:

```
-- Sum integers 0..n
sumTo : I32 -> I32
sumTo n = foldRange 0 n 0 (\acc i -> acc + i)

-- Accumulate 20 particles
color = foldRange 0 20 (vec3 0.0 0.0 0.0) (\acc i ->
  acc + particle uv (toF32 i) time aspect)
```

The function argument can also be a named top-level function:

```
addToAcc : I32 -> I32 -> I32
addToAcc acc i = acc + i

sumNamed : I32 -> I32
sumNamed n = foldRange 0 n 0 addToAcc
```

Prelude signature: `foldRange : I32 -> I32 -> a -> (a -> I32 -> a) -> a`

### 5.13 Record Construction

```
Point { x = 1.0, y = 2.0 }
```

Named record construction. The type is determined by the constructor name (uppercase identifier before `{`).

### 5.14 Record / Bitfield Functional Update

```
point { x = 3.0 }          -- copies point, overriding x
flags { capArrow = 1 }     -- bitfield update
```

Postfix `{ field = expr, ... }` after an expression creates a copy with the specified fields overridden.

### 5.15 Field Access

```
point.x
info.thickness
```

Dot syntax for record field access. Also used for:
- **Vec swizzle**: `v.xy`, `v.xyz`, `v.rgba`, `v.x`
- **Method-call sugar** for in-scope callables: `v.normalize` → `normalize v`

### 5.16 Vec Swizzle

Swizzle patterns use `xyzw` or `rgba` component names (1–4 characters):

```
v.x             -- scalar component
v.xy            -- vec2 from vec3/vec4
v.xyz           -- vec3 from vec4
v.rgba          -- same as v.xyzw
```

The swizzle length determines the result type:
- 1 component → scalar
- 2+ components → vector of that length

### 5.17 Matrix Column Access

Single-character swizzle names on matrices (`mat.x`, `mat.y`, `mat.z`, `mat.w`) access the corresponding **column** as a vector:

```
mat.x             -- first column → Vec<rows, scalar>
mat.y             -- second column → Vec<rows, scalar>
mat[i]            -- index access also returns Vec<rows, scalar>
```

For a `Mat<R, C, T>`, column access returns `Vec<R, T>`. In the MIR, `.x`/`.y` on matrices are lowered to index access with a literal column index (0, 1, 2, 3).

### 5.18 Method-Call Syntax Sugar

```
x.method y      -- desugars to: method x y
a.collapse      -- desugars to: collapse a
```

Any in-scope callable may be used with dot syntax. If a matching `impl` provides the method for the resolved receiver type, that impl method takes priority over a plain function with the same name.

Dot syntax is defined as **receiver-first call sugar**, not as a separate dispatch mechanism. This remains true with multi-parameter traits: expressions such as `x.method y` desugar to `method x y`, and ordinary type inference / constraint solving resolves the call. Dot syntax itself does not privilege receiver-only lookup beyond the surface-level priority rules below.

Priority: **swizzle** > **matching impl method** > **in-scope function call** > **struct field access**.

### 5.19 Index Access

```
arr[i]
buffer[toU32 idx]
```

### 5.20 Vec Literals

```
[1.0, 2.0, 3.0]    -- desugars to: vec3 1.0 2.0 3.0
[x, y]              -- desugars to: vec2 x y
```

### 5.21 Parenthesized Expressions

```
(x + y)
(f x)
(-expr)             -- negation (not operator section)
```

### 5.22 Backtick Infix

```
a `max` b           -- desugars to: max a b
```

Any function can be used as an infix operator by enclosing it in backticks.

### 5.23 Dollar Application

```
f $ g x             -- desugars to: f (g x)
```

The `$` operator has the lowest binding power (right-associative), enabling parenthesis-free chains.

---

## 6. Patterns

### 6.1 Wildcard Pattern

```
_               -- Matches anything, binds nothing
```

### 6.2 Variable Pattern

```
x               -- Matches anything, binds value to x
```

### 6.3 Constructor Pattern

```
Circle r        -- Matches Circle constructor, binds radius to r
Rect w h        -- Matches Rect constructor
Some x          -- Matches Some constructor
None            -- Matches None (no payload)
```

### 6.4 Literal Pattern

```
0               -- Matches integer 0 (polymorphic: works with I32 or U32)
42u             -- Matches U32 value 42
1               -- Matches integer 1
```

Integer literal patterns on I32/U32 scrutinees emit **native WGSL `switch/case`** when all arms are literal patterns without guards.

### 6.5 Or-Pattern (Multi-Value)

```
| 4 | 8  -> ...     -- Matches 4 or 8
| Red | Blue -> ...  -- Matches Red or Blue
```

Or-patterns generate multi-value `case` arms in WGSL switch statements:

```wgsl
case 4i, 8i: { ... }
```

### 6.6 Tuple Pattern

```
(a, b)          -- Destructure a pair
```

Tuple patterns in function parameters are desugared to curried parameters.
In a function definition, a tuple pattern is still one parameter pattern that destructures a tuple argument.

### 6.7 Record Pattern

```
Point { x, y }           -- bind selected fields
Point { x = px, y = py } -- explicit subpatterns
Active { life, .. }      -- bind listed fields, ignore the rest
```

Record patterns destructure named record constructors by field. Field punning is
supported, so `life` means `life = life`. Record patterns are partial matches:
only the listed fields are inspected and bound. Writing `..` makes the ignored
remainder explicit.

### 6.7 As-Pattern

```
x@(Circle r)    -- Bind the whole value to x AND destructure
```

### 6.8 Parenthesized Pattern

```
(Circle r)      -- Grouping
```

---

## 7. Operators

### 7.1 Precedence Table

From **lowest** to **highest** binding power:

| Precedence | Operators | Associativity | Description |
|-----------|-----------|---------------|-------------|
| 1 | `\|>` | Left | Pipeline |
| 1 | `$` | Right | Low-precedence application |
| 1–2 | `\|\|` | Left | Logical OR |
| 3–4 | `&&` | Left | Logical AND |
| 5–6 | `^` | Left | Bitwise XOR |
| 7–8 | `&` | Left | Bitwise AND |
| 9–10 | `==` `/=` `<` `>` `<=` `>=` | Left | Comparison |
| 11–12 | `<<` `>>` | Left | Bit shift |
| 13–14 | `+` `-` | Left | Additive |
| 15–16 | `*` `/` `%` | Left | Multiplicative |
| 19 | (application) | Left | Function application |
| 21 | `-` `!` `~` (prefix), `.field`, `[index]` | — | Unary, access |

### 7.2 Operator Syntax

- Infix operators: `a + b`, `x == y`, `x & y`, `x ^ y`, `x << y`
- Prefix operators: `-x` (negation), `!b` (boolean not), `~x` (bitwise not)
- Operator sections: `(+)` wraps an operator as a function value
- Backtick infix: `` a `f` b `` → `f a b`

### 7.3 Not-Equal Operator

shadml uses `/=` (Haskell-style) for not-equal in source code. It compiles to `!=` in WGSL.

### 7.4 Bitwise Operators

| shadml | WGSL | Description |
|-------|------|-------------|
| `x & y` | `x & y` | Bitwise AND |
| `x ^ y` | `x ^ y` | Bitwise XOR |
| `bor x y` | `x \| y` | Bitwise OR (function; `\|` is reserved for pattern syntax) |
| `x << y` | `x << y` | Shift left |
| `x >> y` | `x >> y` | Shift right (two adjacent `>` with no space between them) |
| `shr x y` | `x >> y` | Shift right (function alternative) |
| `~x` | `~x` | Bitwise NOT |

**Note:** `|` cannot be used as an infix bitwise OR operator because it is reserved for pattern matching guards, match arms, data constructor separators, and or-patterns. Use the `bor` function instead. Similarly, `>>` must be written with no space between the two `>` characters to distinguish it from nested generic type closers (e.g., `Vec<4, Vec<3, F32>>`). The `shr` function is available as an alternative.

### 7.5 Vector Literals

Vector literals use bracket syntax and desugar to WGSL vector constructors:

```
[1.0, 2.0, 3.0]       -- desugars to vec3<f32>(1.0, 2.0, 3.0)
[x, y, z, 1.0]         -- desugars to vec4<f32>(x, y, z, 1.0)
[base.xy, 0.0, 1.0]   -- components can be swizzled vectors
```

The number of components (2–4) is inferred from the elements. Scalar and vector elements can be mixed (e.g., a `vec2` element contributes 2 components).

---

## 8. Type System

### 8.1 Hindley-Milner Type Inference

shadml uses a constraint-based Hindley-Milner type inference engine:

- **Fresh type variables** are generated for inferred expression and pattern types
- **Unification** resolves constraints between types
- **Substitution** maps type variables to concrete types
- **Generalization** creates polymorphic type schemes over variables not free in the environment
- **Instantiation** replaces scheme variables with fresh variables at each use site

### 8.2 Type Unification Rules

- `Var(v)` unifies with any type (occurs check prevents infinite types)
- `Con(a)` unifies with `Con(b)` only if `a == b`
- `Arrow(a1, a2)` unifies with `Arrow(b1, b2)` by unifying components
- `App(f1, a1)` unifies with `App(f2, a2)` by unifying components
- `Tuple(elems1)` unifies with `Tuple(elems2)` if lengths match, by unifying elements
- `Nat(a)` unifies with `Nat(b)` only if `a == b`
- `AssocProj` unifies with any type (permissive — resolution is deferred to predicate solving)
- `Error` unifies with anything (error recovery)

### 8.3 Associated Type Projections

Associated type projections appear in type positions and are resolved to concrete types by looking up matching trait implementations:

```
(Add F32 F32).Output     -- projects the Output associated type of Add for F32 and F32
(Self.Output)            -- within a trait body, refers to the trait's own Output
```

Internally, the type system represents these as `Ty::AssocProj { trait_params, name, trait_name }`. During type inference, `AssocProj` unifies permissively with any type. Concrete resolution happens during predicate solving — when a matching `impl` is found, the projection is replaced with the concrete type defined in that impl.

Ambiguous projections (where no impl can be found or multiple impls match) emit a diagnostic: "ambiguous associated type `.X`".

### 8.4 Type Constructor Normalization

Surface type names are normalized to canonical forms:

| Surface Names | Canonical |
|---------------|-----------|
| `Array`, `Tensor`, `Ten` | `Tensor` |
| `Vec`, `Vector` | `Vec` |
| `Mat`, `Matrix` | `Mat` |
| `Sca`, `Scalar` | `Scalar` (identity: `Scalar F32` = `F32`) |
| `Options`, `Option` | `Option` |

### 8.5 Type Schemes

Polymorphic types are represented as schemes with quantified variables:

```
-- The scheme for `id : a -> a` is:
Scheme { vars: [0], ty: Arrow(Var(0), Var(0)) }
```

Each time a polymorphic name is used, its scheme is instantiated with fresh type variables, enabling type-safe reuse.

### 8.6 Constructor Types

Data type constructors are assigned types during registration:

```
data Option a = Some a | None

-- Some : a -> Option a
-- None : Option a
```

Record constructors take named fields:

```
data Point = Point { x : F32, y : F32 }
-- Point : (record) -> Point
```

---

## 9. Traits and Implementations

### 9.1 Trait Declaration

```
trait TraitName t1 ... tn where
  type AssocTypeName
  methodName : type
  ...
```

Traits define interfaces with method signatures and optional associated type declarations. Associated types are declared with the `type` keyword inside the trait body and referenced via `Self.TypeName` within method signatures:

```
trait Add a b where
  type Output
  (+) : a -> b -> Self.Output
```

Binary operator traits use associated types to describe their result type. The `Self` keyword refers to the trait itself within the trait body, so `Self.Output` means "the `Output` associated type of this trait instance."

### 9.2 Trait Implementation

```
impl TraitName T1 ... Tn where
  type AssocTypeName = ConcreteType;
  methodName args = body
  ...
```

Implementations must provide concrete types for all associated type declarations, followed by method definitions:

```
impl Add Fp64 Fp64 where
  type Output = Fp64;
  (+) a b =
    let s = twoSum a.high b.high
    in quickTwoSum (s.high, s.low + a.low + b.low)
```

Associated type definitions use `type Name = Type;` syntax with a semicolon terminator.

### 9.3 Standalone Implementations

```
impl TypeName where
  methodName : TypeName -> ...
  methodName args = body
```

Standalone `impl` blocks define methods on a type without a trait:

```
impl Fp64 where
  collapse : Fp64 -> F32
  collapse v = v.high + v.low

  neg : Fp64 -> Fp64
  neg a = Fp64 (-a.high) (-a.low)
```

Impl-local method type signatures are optional, but when present they are checked just like top-level signatures. For standalone `impl` blocks, the receiver remains the first argument.

For trait `impl` blocks, the implementation must define every method declared
by the trait. Omitting any required method is a compile-time error.

For a given trait and concrete impl head, at most one trait impl is allowed.
Writing two `impl Trait T1 ... Tn where ...` blocks for the same full head is a
compile-time error.

Trait impl selection is exact-match only over the **full impl head**. The
language does not currently support trait impl specialization, blanket impls,
or overlapping impls. Generic function specialization during lowering is a
separate mechanism and does not affect trait impl selection.

Concretely, a trait impl head may not contain free type variables. For example,
this is rejected:

```
impl Convert (Vec<3, a>) where
  convert v = ...
```

with a diagnostic in the spirit of:

```
Trait impl heads must be concrete: blanket impls like `impl Convert ...` are not supported
```

### 9.4 Associated Type Projections

Outside of trait bodies, associated types are referenced using dot-projection syntax on the trait parameters:

```
(Add a b).Output       -- the Output of Add for types a and b
(Add F32 F32).Output    -- resolves to F32
(Add Vec3 F32).Output   -- resolves to Vec3
```

The type system represents these as `Ty::AssocProj { trait_params, name, trait_name }`. During inference, `AssocProj` unifies permissively with any type. Resolution occurs during predicate solving — when a matching `impl` is found, the projection is replaced with the concrete type.

Within trait bodies, `Self.TypeName` is used to reference the trait's own associated types.

### 9.5 Operator Overloading

**Arithmetic operator traits:** `Add` (`+`), `Sub` (`-`), `Mul` (`*`), `Div` (`/`), `Mod` (`%`).

**Bitwise operator traits:** `BitAnd` (`&`), `BitXor` (`^`), `Shl` (`<<`), `Shr` (`shr`), `BitNot` (`bitnot`), `Neg` (`negate`).

Binary operator traits use **associated types** for their result type. The prelude defines them as:

```
trait Add a b where
  type Output
  (+) : a -> b -> Self.Output

trait Sub a b where
  type Output
  (-) : a -> b -> Self.Output

trait Mul a b where
  type Output
  (*) : a -> b -> Self.Output

trait Div a b where
  type Output
  (/) : a -> b -> Self.Output

trait Mod a b where
  type Output
  (%) : a -> b -> Self.Output

trait BitAnd a b where
  type Output
  (&) : a -> b -> Self.Output

trait BitXor a b where
  type Output
  (^) : a -> b -> Self.Output

trait Shl a b where
  type Output
  (<<) : a -> b -> Self.Output

trait Shr a b where
  type Output
  shr : a -> b -> Self.Output
```

Unary operator traits use a single parameter:

```
trait BitNot a where
  type Output
  bitnot : a -> Self.Output

trait Neg a where
  type Output
  negate : a -> Self.Output
```

Operators on primitive types (I32, U32, F32) use native WGSL operators via `builtin impl` declarations. Operators on user-defined types dispatch through trait implementations:

```
impl Add Fp64 Fp64 where
  type Output = Fp64;
  (+) a b = ...

-- Now x + y where x, y : Fp64 calls the trait method

impl BitAnd Mask Mask where
  type Output = Mask;
  (&) a b = Mask { bits = a.bits & b.bits }

-- Now a & b where a, b : Mask calls bitand_Mask

impl BitNot Mask where
  type Output = Mask;
  bitnot a = Mask { bits = ~a.bits }

-- Now ~a where a : Mask calls bitnot_Mask

impl Neg Wrapper where
  type Output = Wrapper;
  negate a = Wrapper { val = -a.val }

-- Now -a where a : Wrapper calls negate_Wrapper
```

The associated type design allows heterogeneous operator relations while keeping the trait parameter list focused on operand types. The result type is determined by the impl:

```
-- Vec * scalar → Vec
impl Mul (Vec 3 F32) F32 where
  type Output = Vec 3 F32;
  (*) v s = v * splat3 s

-- scalar * Vec → Vec
impl Mul F32 (Vec 3 F32) where
  type Output = Vec 3 F32;
  (*) s v = splat3 s * v
```

The operator method syntax uses parenthesized operator names: `(+)`, `(-)`, `(*)`, `(&)`, `(^)`, `(<<)`, etc. Non-operator trait methods like `shr`, `bitnot`, and `negate` use plain names.

### 9.6 Dispatch Mechanism

Generic functions that use trait-dispatched operations may infer predicates
during expression typing. For example:

```
add x y = x + y
```

infers a constrained type in the shape:

```
add : Add a b => a -> b -> (Add a b).Output
```

For top-level bindings, such constrained types must be written explicitly:

```
add : Add a b => a -> b -> (Add a b).Output
add x y = x + y
```

The same rule applies to ordinary trait methods:

```
lighting : Light a => a -> Vec<3, F32> -> Vec<3, F32>
lighting light = position light
```

Omitting the constraint on a constrained top-level binding is a type error.
Local `let` / `where` bindings may retain inferred constrained schemes without
an explicit signature in the current design.

Trait dispatch is **fully static** — no vtables or runtime dispatch. Impl methods are compiled as regular functions with mangled names (e.g., `add_Fp64`). At trait resolution time, `Var(method)` is rewritten to `Var(mangled_name)` based on the resolved type of the operands.

Associated type projections are resolved during lowering. When a concrete impl is found, `(Add T1 T2).Output` is replaced by the concrete type defined in the impl's `type Output = ...` definition.

Trait impl resolution uses the fully resolved concrete **full impl head** and
requires an exact match. There is no partial-ordering rule between impls
because specialized or overlapping trait impls are not part of the current
language.

If two impl heads for the same trait would both match the same concrete type,
the program is rejected with a diagnostic in the spirit of:

```
Overlapping implementation of trait 'Convert' for type 'Vec<3, F32>'
```

---

## 10. Module System

### 10.1 Module Structure

Each file is a module. Directory structure defines namespaces:

```
src/
  Math/
    Fp64.shadml      -- module Math.Fp64
    Utils.shadml      -- module Math.Utils
  Main.shadml         -- module Main
```

### 10.2 Module Header

```
module Math.Fp64
```

Optional. If absent, the module name is derived from the file path relative to source roots.

### 10.3 Import Forms

| Syntax | Description |
|--------|-------------|
| `import Foo` | Import all public names from Foo |
| `import Foo (bar, baz)` | Import only `bar` and `baz` |
| `import Foo as F` | Qualified access: `F.bar` |
| `import Foo.*` | Import all sub-modules under Foo/ |
| `import Foo when cfg.debug` | Conditional import |

### 10.4 Visibility

Everything is **public by default**. There is a planned `private` keyword for module-local declarations (not yet implemented).

### 10.5 Module Resolution

The module resolver (`shadml_parser::module_resolver`) resolves imports to source files:

1. Builds a `ModuleGraph` from the root file's imports
2. Reads imported files via a `SourceReader` trait (supports filesystem or `VirtualFs`)
3. Performs topological sort with cycle detection
4. Merges modules in dependency order

### 10.6 Virtual Filesystem

`VirtualFs` provides an in-memory filesystem for browser/WASM/testing:

```rust
let mut vfs = VirtualFs::new();
vfs.add("Math/Fp64.shadml", source_code);
```

### 10.7 Bundle Format

For single-file multi-module embedding, `parse_bundle()` parses section markers:

```
--- module Math.Fp64 ---
... declarations ...

--- module Main ---
... declarations ...
```

---

## 11. Conditional Compilation

### 11.1 Feature Flags

Features are enabled via the CLI:

```
shadml compile file.shadml --feature debug --feature aa
```

Features are referenced in source as `cfg.name`.

### 11.2 Block Form

```
when cfg.debug
  debugLog : F32 -> ()
  debugLog x = ...
```

Declarations in the `when` body must be indented further than the `when` keyword.

**Limitation:** Each `when` block supports one type signature + one function definition pair. For multiple functions, use separate `when` blocks.

### 11.3 Else / Else-When

```
when cfg.aa
  calculateAA : F32 -> F32
  calculateAA x = ...
else
  calculateAA : F32 -> F32
  calculateAA x = 1.0
```

Chained:

```
when cfg.tier3
  maxLights : I32
  maxLights = 64
else when cfg.tier2
  maxLights : I32
  maxLights = 16
else
  maxLights : I32
  maxLights = 4
```

### 11.4 Conditional Imports

```
import Debug when cfg.debug
```

### 11.5 Predicate Combinators

| Syntax | Description |
|--------|-------------|
| `cfg.name` | Feature flag is set |
| `not pred` | Negation (tightest binding) |
| `pred && pred` | Conjunction |
| `pred \|\| pred` | Disjunction (loosest binding) |

Example:

```
when cfg.debug && cfg.msaa
  debugSamples : I32
  debugSamples = 4
```

### 11.6 Feature Evaluation

`FeatureSet::from_flags(&[String])` creates the feature set. `evaluate_features(&mut Program, &FeatureSet)` prunes the AST in-place after parsing and before module resolution — declarations in unfulfilled `when` branches are removed entirely from the AST.

---

## 12. Bitfields

Bitfields provide packed integer flag words with named fields.

### 12.1 Declaration

```
bitfield Name : BaseType = ConstructorName {
  field1 : kind1,
  field2 : kind2,
  ...
}
```

The constructor name appears after `=`, mirroring `data` record syntax. Fields support four forms:

| Syntax | Meaning | Accessor returns |
|---|---|---|
| `name : Type : N` | Typed field with explicit bit width | `Type` |
| `name : Bool` | Boolean field, always 1 bit | `Bool` |
| `name : EnumType` | Enum-typed, width inferred from variant count | `EnumType` |
| `name : N` | Bare integer width | base type (U32) |

Example:

```
bitfield CapFlags : U32 = CapFlags {
  endCap   : Bool,
  startCap : Bool,
  capButt  : Bool,
  capRound : Bool,
  capArrow : Bool,
}
```

### 12.2 Typed Fields

Fields can specify an explicit type and bit width:

```
bitfield LineFlags : U32 = LineFlags {
  capStart  : CapStyle : 2,    -- CapStyle enum stored in 2 bits
  capEnd    : CapStyle : 2,
  roughness : U32 : 5,         -- 5-bit unsigned integer
  visible   : Bool,            -- 1 bit (width implicit)
  mode      : BlendMode,       -- width inferred from enum variant count
}
```

For `Type : N` fields where `Type` is an enum, the compiler validates that `N >= ceil(log2(variant_count))`.

`Bool` always maps to width 1.

Bare integer widths (`name : N`) remain supported for quick prototyping.

### 12.3 Width Validation

The compiler checks at compile time that:
- The total bit width of all fields does not exceed the base type width (32 bits for U32)
- Typed fields have sufficient bits for their declared type

### 12.4 Field Access

```
flags.capRound      -- extract field via shift and mask: (flags >> offset) & mask
```

1-bit fields produce a boolean result: `(val >> offset & 1u) != 0u`.

### 12.5 Construction

```
CapFlags { endCap = 1, startCap = 0, capButt = 0, capRound = 1, capArrow = 0 }
```

Compiles to OR-chain of shifted masked values. For 1-bit fields with boolean values, uses `select(0u, 1u, val)`. For integer values, casts to U32 directly.

### 12.6 Functional Update

```
flags { capArrow = 1 }     -- clear field bits, then OR in new value
```

Compiles to: `(base & ~combined_mask) | ((new_val & field_mask) << offset)`.

### 12.7 Implementation Notes

- Bitfields are **not** registered as type aliases — they remain opaque so `FieldAccess` preserves the type name
- No WGSL structs are emitted for bitfields — they lower to plain integer operations
- Bitfield info is threaded through `LowerCtx` in HIR→MIR lowering
- `impl` blocks work on bitfield types (they are `Ty::Con("Name")` in the type system)

---

## 13. Entry Points and GPU Resources

### 13.1 Shader Stages

Compute shaders are declared at module scope with `@compute`:

```
main : ComputeInput -> ()
@compute @workgroup_size(64, 1, 1)
main input = ...
```

Vertex and fragment shaders must be declared inside a `render` block (see 13.2). Module-scope `@vertex` and `@fragment` entry points are rejected with a compile-time error.

```
render gradient
  vsMain : VertexInput -> VertexOutput
  @vertex
  vsMain input = ...

  fsMain : VertexOutput -> Vec<4, F32>
  @fragment
  fsMain input = ...
```

### 13.2 Render Blocks

A `render` block groups a vertex/fragment pipeline pair together with the bindings they share. Bindings declared inside a render block are scoped to that pipeline and receive `VERTEX | FRAGMENT` visibility in the generated pipeline layout.

**Syntax:**

```
render name
  @group(G)
    @binding(B) address_space name : Type

  @vertex
  vsMain : VertexInput -> VertexOutput
  vsMain input = ...

  @fragment
  fsMain : VertexOutput -> Vec<4, F32>
  fsMain input = ...
```

- `render` is a layout-triggering keyword (like `let` and `where`). The body is indentation-scoped; no braces are required.
- `name` identifies the render block for generated pipeline layout helpers.
- The block may contain:
  - **Binding declarations** (`@group`/`@binding` globals, `immediate` globals) — scoped to this pipeline
  - **Entry points** — one `@vertex` and one `@fragment` function
  - **Type signatures** preceding entry points
  - **Data declarations** (`data`, `enum`, `bitfield`) needed by the entry points
- A module may contain any number of render blocks. This is useful for multi-pass rendering (e.g. one render block that draws into an MSAA texture, and a second that resolves it to the screen).
- Compute entry points (`@compute`) may not appear inside a render block.

**Example — multi-pass rendering:**

```
render shape
  @vertex
  vsShape : ShapeInput -> ShapeOutput
  vsShape input = ...

  @fragment
  fsShape : ShapeOutput -> Vec<4, F32>
  fsShape input = ...

render resolve
  @group(1)
    @binding(0) msTexture : Texture2dMultisampled F32

  @vertex
  vsResolve : FullscreenInput -> FullscreenOutput
  vsResolve input = ...

  @fragment
  fsResolve : FullscreenOutput -> Vec<4, F32>
  fsResolve input = ...
```

**Interaction with imports:**

Bindings imported from other modules (via `import`) are available inside render blocks by referencing them normally. The render block only needs to declare bindings that are specific to that pipeline; shared bindings can live in a dedicated module and be imported at module scope.

```
import GlobalBindings

render effects
  @group(1)
    @binding(0) mainTexture : Texture2d F32
    @binding(1) mainSampler : Sampler

  @vertex
  vsMain : VertexInput -> VertexOutput
  vsMain input = ...

  @fragment
  fsMain : VertexOutput -> Vec<4, F32>
  fsMain input =
    let color = textureSample mainTexture mainSampler input.uv
    in color
```

### 13.3 Struct-Based I/O

Entry points use struct-based I/O. Input and output structs carry `@builtin` and `@location` attributes on their fields:

```
data ComputeInput = ComputeInput {
  @builtin(global_invocation_id) gid : Vec<3, U32>
}

data VertexOutput = VertexOutput {
  @builtin(position) clip_position : Vec<4, F32>,
  @location(0)       color         : Vec<4, F32>,
}
```

### 13.4 Return Type Annotations

For vertex/fragment entry points returning non-struct types (e.g., `Vec<4, F32>`), the codegen automatically emits `@location(0)` on the return type:

```wgsl
@fragment
fn fsMain(input: VertexOutput) -> @location(0) vec4<f32> { ... }
```

### 13.5 Resource Bindings

```
@group(G) @binding(B) uniform             name : T
@group(G) @binding(B) storage(read_write) name : Array<T>
@group(G) @binding(B) storage              name : Array<T>
@group(G) @binding(B) immediate            name : T
@group(G) @binding(B)                      name : Texture2d F32
@group(G) @binding(B)                      name : Sampler
```

Compiles to WGSL:

```wgsl
@group(G) @binding(B) var<uniform> name: T;
@group(G) @binding(B) var<storage, read_write> name: array<T>;
@group(G) @binding(B) var<storage, read> name: array<T>;
@group(G) @binding(B) var<immediate> name: T;
@group(G) @binding(B) var name: texture_2d<f32>;
@group(G) @binding(B) var name: sampler;
```

### 13.6 Push Constants (Immediates)

The `immediate` address space maps to WGSL `var<immediate>`, providing small read-only data passed per-draw or per-dispatch call. This is shadml's equivalent of WebGPU push constants:

```
data PushConstants = PushConstants {
  color : Vec<4, F32>,
  time  : F32,
}

@group(0) @binding(0) immediate imm : PushConstants

main : ComputeInput -> ()
@compute @workgroup_size(64, 1, 1)
main input =
  let color = imm.color
  in ...
```

Compiles to:

```wgsl
struct PushConstants {
  color : vec4<f32>,
  time  : f32,
}

var<immediate> imm : PushConstants;
```

Push constant size limits and alignment follow the WebGPU/WGSL specification for the `immediate` address space.

### 13.7 Texture and Sampler Bindings

Textures and samplers use the opaque binding address space (no keyword before the name):

```
@group(0) @binding(0) myTexture  : Texture2d F32
@group(0) @binding(1) mySampler  : Sampler
@group(0) @binding(2) myTexArray : BindingArray (Texture2d F32) 8
@group(0) @binding(3) mySampArray : BindingArray Sampler 4
```

Compiles to:

```wgsl
@group(0) @binding(0) var myTexture: texture_2d<f32>;
@group(0) @binding(1) var mySampler: sampler;
@group(0) @binding(2) var myTexArray: binding_array<texture_2d<f32>, 8>;
@group(0) @binding(3) var mySampArray: binding_array<sampler, 4>;
```

#### Sampler State Hints

Sampler bindings may carry an `@samplerState` hint attribute that controls both the generated `wgpu::SamplerDescriptor` and the `wgpu::BindingType` for the bind group layout entry:

```
@group(0) @binding(1)
@samplerState(filter = "nearest", address_mode_u = "clamp_to_edge")
mySampler : Sampler
```

Supported fields:

| Field | Values | Default |
|-------|--------|---------|
| `filter` | `"linear"`, `"nearest"` | `"linear"` |
| `mipmap_filter` | `"linear"`, `"nearest"` | `"linear"` |
| `address_mode_u` | `"repeat"`, `"clamp_to_edge"`, `"mirror_repeat"` | `"repeat"` |
| `address_mode_v` | `"repeat"`, `"clamp_to_edge"`, `"mirror_repeat"` | `"repeat"` |
| `address_mode_w` | `"repeat"`, `"clamp_to_edge"`, `"mirror_repeat"` | `"repeat"` |

- `filter = "nearest"` causes the generated `BindingType` to be `SamplerBindingType::NonFiltering`; otherwise it is `Filtering`.
- `SamplerComparison` bindings always use `Comparison` regardless of hints.
- `@samplerState` is stripped during WGSL lowering and does not appear in the generated shader.
- The bindgen layer emits a `create_{name}_sampler(device)` helper function for each sampler binding that carries `@samplerState` hints.

#### Texture Sample Type Hints

Texture bindings may carry a `@textureSampleType` hint attribute that controls the `sample_type` field of the generated `wgpu::BindingType::Texture` entry:

```
@group(0) @binding(0)
@textureSampleType(filterable = false)
myTex : Texture2d F32
```

Supported fields:

| Field | Values | Default | Applicable to |
|-------|--------|---------|---------------|
| `filterable` | `true`, `false` | `true` | `Texture2d F32`, `Texture2dArray F32` only |

- The `sample_type` is automatically derived from the texture's element type:
  - `Texture2d F32` → `Float { filterable: true }` (default)
  - `Texture2d F32` + `@textureSampleType(filterable = false)` → `Float { filterable: false }`
  - `Texture2d I32` → `Sint`
  - `Texture2d U32` → `Uint`
- `Texture2dMultisampled` is always `Float { filterable: false }` regardless of hints.
- The `filterable` field is only valid when the texture's element type is `F32`; using it on `I32` or `U32` textures produces a compile error.
- `@textureSampleType` is stripped during WGSL lowering and does not appear in the generated shader.

### 13.7 Resource Operations

| shadml | WGSL | Description |
|-------|------|-------------|
| `load resource` | `resource` (identity) | Read from uniform/storage |
| `writeAt resource index value` | `resource[index] = value` | Write to storage buffer |

---

## 14. Prelude and Builtins

The prelude (`prelude/prelude.shadml`) is loaded via `include_str!` and prepended to every compilation. It provides type signatures for built-in operations.

### 14.1 Prelude Data Types

```
data Option a = Some a | None
data Result a = Ok a | Err String
data Pair a b = Pair a b
```

### 14.2 Operator Traits

**Arithmetic:**
```
trait Add a b where
  type Output
  (+) : a -> b -> Self.Output

trait Sub a b where
  type Output
  (-) : a -> b -> Self.Output

trait Mul a b where
  type Output
  (*) : a -> b -> Self.Output

trait Div a b where
  type Output
  (/) : a -> b -> Self.Output

trait Mod a b where
  type Output
  (%) : a -> b -> Self.Output
```

**Bitwise:**
```
trait BitAnd a b where
  type Output
  (&) : a -> b -> Self.Output

trait BitXor a b where
  type Output
  (^) : a -> b -> Self.Output

trait Shl a b where
  type Output
  (<<) : a -> b -> Self.Output

trait Shr a b where
  type Output
  shr : a -> b -> Self.Output

trait BitNot a where
  type Output
  bitnot : a -> Self.Output

trait Neg a where
  type Output
  negate : a -> Self.Output
```

Binary operator traits take two type parameters (the operand types) and declare an associated `Output` type. Unary operator traits take one type parameter. This allows heterogeneous operator relations while keeping the result type determined by the implementation.

### 14.3 Arithmetic Operators

```
extern (+) : a -> a -> a
extern (-) : a -> a -> a
extern (*) : a -> a -> a
extern (/) : a -> a -> a
extern (%) : a -> a -> a
```

These declarations provide the surface operator names used during parsing and
inference. The actual trait constraints that arise from use may be
heterogeneous, depending on the resolved operator relation.

### 14.4 Comparison Operators

```
extern (==) : a -> a -> Bool
extern (/=) : a -> a -> Bool
extern (<)  : a -> a -> Bool
extern (>)  : a -> a -> Bool
extern (<=) : a -> a -> Bool
extern (>=) : a -> a -> Bool
```

### 14.5 Logical Operators

```
extern (&&) : Bool -> Bool -> Bool
extern (||) : Bool -> Bool -> Bool
```

### 14.6 Bitwise Operators

```
extern (&)  : a -> a -> a   -- bitwise AND
extern (^)  : a -> a -> a   -- bitwise XOR
extern (<<) : a -> a -> a   -- shift left
extern bor  : a -> a -> a   -- bitwise OR (| conflicts with pattern syntax)
extern shr  : a -> a -> a   -- shift right (>> works as infix with no space)
```

Prefix bitwise NOT (`~x`) is a built-in prefix operator; no `extern` needed.

### 14.7 Loop Combinators

```
extern foldRange : I32 -> I32 -> a -> (a -> I32 -> a) -> a
```

`foldRange start end init f` folds `f` over the integer range `[start, end)`, threading an accumulator. Compiles to an efficient WGSL `loop` with mutable variables.

### 14.8 Math Functions (Unary)

```
sin  cos  abs  fract  floor  sign  sqrt  log  log2  exp  ceil  round  trunc  negate
saturate  inverseSqrt  asin  acos  sinh  cosh  tanh  asinh  acosh  atanh
```

All have type `a -> a`.

### 14.9 Math Functions (Binary)

```
max  min  step  mod  pow  reflect  atan  atan2  ldexp
```

All have type `a -> a -> a` except `ldexp` (which takes a scalar and an integer exponent).

### 14.10 Math Functions (Ternary)

```
clamp     : a -> a -> a -> a
mix       : a -> a -> b -> a
smoothstep : a -> a -> b -> a
fma       : a -> a -> a -> a
```

### 14.11 Vector Operations

```
normalize : Vec<n, a> -> Vec<n, a>
length    : Vec<n, a> -> a
dot       : Vec<n, a> -> Vec<n, a> -> a
distance  : Vec<n, a> -> Vec<n, a> -> a
cross     : Vec<3, a> -> Vec<3, a> -> Vec<3, a>
select    : a -> a -> Bool -> a
faceForward : Vec<n, a> -> Vec<n, a> -> Vec<n, a> -> Vec<n, a>
refract   : Vec<n, a> -> Vec<n, a> -> a -> Vec<n, a>
```

### 14.12 Vector Component Access

```
vecX : Vec<n, a> -> a
vecY : Vec<n, a> -> a
vecZ : Vec<n, a> -> a
vecW : Vec<n, a> -> a
```

Extract the x/y/z/w component of a vector.

### 14.13 Packing / Unpacking

```
unpack4x8unorm   : U32 -> Vec<4, F32>
pack4x8unorm     : Vec<4, F32> -> U32
unpack4x8snorm   : U32 -> Vec<4, F32>
pack4x8snorm     : Vec<4, F32> -> U32
unpack2x16float  : U32 -> Vec<2, F32>
pack2x16float    : Vec<2, F32> -> U32
unpack2x16unorm  : U32 -> Vec<2, F32>
pack2x16unorm    : Vec<2, F32> -> U32
unpack2x16snorm  : U32 -> Vec<2, F32>
pack2x16snorm    : Vec<2, F32> -> U32
```

### 14.14 Vector Constructors

```
vec2 : a -> a -> Vec<2, a>
vec3 : a -> a -> a -> Vec<3, a>
vec4 : a -> a -> a -> a -> Vec<4, a>
splat2 : a -> Vec<2, a>
splat3 : a -> Vec<3, a>
splat4 : a -> Vec<4, a>
```

### 14.15 Fragment Shader Derivatives

```
dpdx  dpdy  dpdxCoarse  dpdxFine  dpdyCoarse  dpdyFine  fwidth  fwidthCoarse  fwidthFine
```

All have type `a -> a`.

### 14.16 Type Cast Builtins

| shadml | WGSL | Type |
|-------|------|------|
| `toF32 x` | `f32(x)` | `a -> F32` |
| `toI32 x` | `i32(x)` | `a -> I32` |
| `toU32 x` | `u32(x)` | `a -> U32` |
| `toBool x` | `bool(x)` | `a -> Bool` |

### 14.17 Resource Operations

| shadml | Behavior |
|-------|----------|
| `load x` | Identity (reads resource value) |
| `writeAt buf idx val` | `buf[idx] = val` |

### 14.18 Angle Conversion

```
radians : a -> a    -- degrees to radians
degrees : a -> a    -- radians to degrees
```

### 14.19 Integer Bit Operations

```
countOneBits         : a -> a    -- counts set bits (popcount)
countLeadingZeros    : a -> a    -- counts leading zero bits
countTrailingZeros  : a -> a    -- counts trailing zero bits
reverseBits          : a -> a    -- reverses bits
firstTrailingBit    : a -> a    -- finds first trailing set bit
firstLeadingBit     : a -> a    -- finds first leading set bit
extractBits          : a -> a -> a -> a    -- extracts bit field
insertBits           : a -> a -> a -> a -> a    -- inserts bit field
```

### 14.20 Synchronization Barriers

```
storageBarrier   : () -> ()    -- storageBarrier() in WGSL
workgroupBarrier : () -> ()    -- workgroupBarrier() in WGSL
```

### 14.21 Texture Operations

| shadml | WGSL | Type |
|-------|------|------|
| `textureSample t s coords` | `textureSample(t, s, coords)` | Texture sampling |
| `textureSampleArray t s coords idx` | `textureSample(t, s, coords, idx)` | Texture array sampling |
| `textureLoad t coords` | `textureLoad(t, coords)` | Texel read (no sampler) |
| `textureLoadMsaa t coords sample` | `textureLoad(t, coords, sample)` | Multisampled texel read |
| `textureLoadArray t coords idx` | `textureLoad(t, vec3(coords, idx))` | Texture array texel read |
| `textureStore t coords val` | `textureStore(t, coords, val)` | Write texel |
| `textureDimensions t` | `textureDimensions(t)` | Texture size |
| `textureDimensionsMsaa t` | `textureDimensions(t)` | Multisampled texture size |
| `textureDimensionsArray t` | `textureDimensions(t)` | Texture array size |

### 14.22 Atomic Operations

```
atomicLoad    : a -> a                 -- atomic load
atomicStore   : a -> a -> ()           -- atomic store
atomicAdd     : a -> a -> a            -- atomic add (returns old value)
atomicSub     : a -> a -> a            -- atomic subtract
atomicMax     : a -> a -> a            -- atomic max
atomicMin     : a -> a -> a            -- atomic min
atomicAnd     : a -> a -> a            -- atomic AND
atomicOr      : a -> a -> a            -- atomic OR
atomicXor     : a -> a -> a            -- atomic XOR
atomicExchange : a -> a -> a           -- atomic exchange
```

### 14.23 Array Length

```
arrayLength : Array<a> -> I32    -- arrayLength() in WGSL
```

---

## 15. Compilation Pipeline

```
Source → Parse → Feature Eval → Module Resolution → Module Merge
     → Semantic Analysis → AST→HIR Lowering → HIR→MIR Lowering
     → Dead Code Elimination → WGSL Code Generation
```

### 15.1 Parsing

- Lexer tokenizes source into `Token` stream
- Layout resolver inserts virtual indentation tokens
- Parser produces AST (`Program` with `Vec<Decl>`)

### 15.2 Feature Evaluation

`evaluate_features()` prunes `CfgDecl` nodes based on active `--feature` flags, removing dead branches from the AST.

### 15.3 Module Resolution

If the program has `import` declarations, the module resolver:
1. Discovers and parses imported files
2. Builds a dependency graph
3. Topologically sorts modules
4. Merges into a single flat `Program`

### 15.4 Semantic Analysis

The `SemanticAnalyzer`:
- Registers data types and their constructors
- Registers type aliases
- Registers traits and impls (including associated type declarations and definitions)
- Performs name resolution
- Runs Hindley-Milner type inference on all expressions
- Resolves associated type projections via predicate solving
- Validates type correctness of all match arms, guards, let bindings
- Desugars tuple parameters and call sites
- Handles method-call sugar resolution

### 15.5 AST → HIR Lowering

The `AstLowering` phase:
- Lowers AST expressions to typed HIR expressions
- Resolves trait methods to mangled concrete function names
- Resolves associated type projections to concrete types via impl lookup
- Resolves operator overloading (BinOp → App for user-defined types)
- Beta-reduces lambda applications
- Desugars pipeline `|>` to function application
- Desugars method-call syntax to function application
- Specializes generic functions at concrete call sites
- Resolves bitfield operations
- Runs a finalization pass to apply all type substitutions

### 15.6 HIR → MIR Lowering

The MIR lowering phase:
- Converts functional expressions to imperative statements
- Lowers `let` bindings to `MirStmt::Let`
- Lowers `if/then/else` to `MirStmt::If`
- Lowers `match/case` to either native `switch/case` or if-else chains
- Lowers `loop` to `MirStmt::Loop` with `var` + `continue` + `break`
- Lowers bitfield access to shift/mask operations
- Lowers bitfield construction/update to bit manipulation
- Converts specialized generic functions to plain WGSL functions
- Converts `toF32`/`toI32`/`toU32`/`toBool` to `MirExpr::Cast`
- Converts `load` to identity, `writeAt` to `MirStmt::IndexAssign`

### 15.7 Dead Code Elimination

`reachability::eliminate_dead_code()` removes functions not reachable from any entry point.

### 15.8 WGSL Code Generation

The codegen emits valid WGSL text:
- Structs with field types and attributes
- Global resource bindings with address spaces
- Module-level constants
- Functions with typed parameters and return types
- Entry points with stage attributes and workgroup sizes
- `@location(0)` on non-struct return types of vertex/fragment entry points
- Identifier sanitization (WGSL reserved words)

---

## 16. WGSL Code Generation

### 16.1 Type Mapping

| shadml | WGSL |
|-------|------|
| `I32` | `i32` |
| `U32` | `u32` |
| `F32` | `f32` |
| `Bool` | `bool` |
| `Vec<N, T>` | `vecN<T>` |
| `Mat<R, C, T>` | `matRxC<T>` |
| `Array<T, N>` | `array<T, N>` |
| `Array<T>` | `array<T>` |
| `Texture2d F32` | `texture_2d<f32>` |
| `Texture2dMultisampled F32` | `texture_2d<f32>` |
| `Texture2dArray F32` | `texture_2d_array<f32, num>` |
| `Sampler` | `sampler` |
| `SamplerComparison` | `sampler_comparison` |
| `BindingArray T N` | `binding_array<T, N>` |
| `()` | (no return type) |
| User struct | `StructName` |

### 16.2 ADT Encoding

**Pure enums** (all constructors have no fields):

```
data Color = Red | Green | Blue
-- Compiles to: u32 values (Red=0, Green=1, Blue=2)
```

**Sum types** with fields:

```
data Shape = Circle F32 | Rect F32 F32
-- Compiles to:
struct Shape {
  tag: u32,
  field0: f32,
  field1: f32,
}
```

### 16.3 Match Compilation

**Integer literal match** (no guards, all literal/or patterns): native WGSL `switch/case`:

```wgsl
switch (_scrut) {
  case 1i: { ... }
  case 2i, 3i: { ... }    // multi-value from or-patterns
  default: { ... }
}
```

**Other matches**: if-else chain with tag checks and field extraction.

**When-guards**: nested `if (guard) { body } else { fallthrough }` inside pattern match conditions.

### 16.4 Loop Compilation

```
loop go (i = 0) (acc = 0) in
  if i < n then go (acc + i) (i + 1) else acc
```

Compiles to:

```wgsl
var i: i32 = 0i;
var acc: i32 = 0i;
var _result: i32 = 0i;
loop {
  if (i < n) {
    let _tmp_0 = acc + i;
    let _tmp_1 = i + 1i;
    i = _tmp_0;    // temp lets avoid ordering issues
    acc = _tmp_1;
    continue;
  } else {
    _result = acc;
    break;
  }
}
```

---

## 17. Tooling

### 17.1 Crate Architecture

| Crate | Purpose |
|-------|---------|
| `shadml_syntax` | Token kinds and SyntaxKind enum |
| `shadml_span` | Source span tracking |
| `shadml_diagnostics` | Diagnostic infrastructure (errors, warnings) |
| `shadml_allocator` | Arena allocator |
| `shadml_cst` | Concrete syntax tree (placeholder) |
| `shadml_ast` | Abstract syntax tree (placeholder) |
| `shadml_parser` | Lexer, layout resolver, parser, module resolver, feature eval |
| `shadml_typechecker` | Type representation, unification, inference engine |
| `shadml_semantic` | Semantic analysis (name resolution, type inference) |
| `shadml_ast_lowering` | AST → HIR lowering |
| `shadml_hir` | High-level IR (typed, desugared) |
| `shadml_mir` | Mid-level IR (imperative, WGSL-close) + HIR→MIR lowering |
| `shadml_wgsl_codegen` | MIR → WGSL text emission |
| `shadml_bundler` | Multi-file compilation, dependency resolution, entry-point splitting |
| `shadml_bindgen` | Rust code generation (GPU structs, binding reflection, pipeline helpers) |
| `shadml_ide` | IDE features (completions, hover, goto-def, references) |
| `shadml_formatter` | Token-stream based code formatter |
| `shadml_language_server` | LSP server (tower-lsp over stdin/stdout) |
| `shadml_wasm` | WASM compilation target (compile, parse, format, diagnostics, IDE) |
| `shadml_cli` | Command-line interface |
| `shadml_integration_tests` | End-to-end compiler tests |

### 17.2 CLI

```
shadml compile <file>    Compile .shadml to .wgsl (stdout)
shadml check <file>      Type-check without emitting
shadml fmt <file>        Format source code
shadml version           Print version
shadml help              Print help
```

**Options:**

| Flag | Description |
|------|-------------|
| `--emit-ast` | Print AST debug output |
| `--preserve-comments` | Preserve source comments in WGSL output |
| `--feature <name>` | Enable a compile-time feature flag (repeatable) |

### 17.3 Language Server (LSP)

The `shadml-lsp` binary provides a Language Server Protocol server over stdin/stdout (via tower-lsp + tokio).

**Supported features:**
- Diagnostics (parse errors, type errors)
- Completions (context-aware)
- Hover (type information)
- Go-to-definition
- Find references
- Semantic tokens
- Formatting

### 17.4 Formatter

The `shadml_formatter` crate provides token-stream based formatting:

```rust
format_default(source: &str) -> String
format(source: &str, config: &FormatConfig) -> String
```

`FormatConfig` supports configurable indentation width (default: 2).

### 17.5 WASM Target

The `shadml_wasm` crate compiles to `wasm32-unknown-unknown` and exports:

| Function | Description |
|----------|-------------|
| `compile(source)` | Full compilation to WGSL |
| `parse_ast(source)` | Parse and return AST debug string |
| `format(source)` | Format source code |
| `get_diagnostics(source)` | Return diagnostics as JSON |
| `editor_completions(source, line, col)` | Completions at position |
| `editor_hover(source, line, col)` | Hover info at position |
| `editor_definition(source, line, col)` | Go-to-definition |
| `editor_references(source, ...)` | Find references |

### 17.6 Editor Integrations

**VS Code** (`editors/vscode/`)
- TextMate grammar for syntax highlighting
- LSP client that spawns `shadml-lsp`

**Helix** (`editors/helix/`)
- `languages.toml` configuration
- Tree-sitter query files

**Zed** (`editors/zed/`)
- `extension.toml` + `config.toml`
- Query files for highlights, indents, outline, brackets

**Tree-sitter** (`tree-sitter-shadml/`)
- `grammar.js` with query files (highlights, locals, indents, textobjects)
- Excluded from workspace (separate build)
- Note: Tree-sitter has limitations with indentation-sensitive multi-line constructs (no external scanner)
- LSP semantic tokens provide the most accurate highlighting

### 17.7 Bundler

The `shadml_bundler` crate compiles multi-file shadml projects:

- Reads a `shadml.toml` configuration file
- Resolves module imports and dependencies
- Compiles all entry-point shaders through the full pipeline
- Supports splitting output by entry point (`split_entry_points = true`)
- Outputs compiled WGSL to a configurable `output_dir`

### 17.8 Bindgen

The `shadml_bindgen` crate generates Rust source code from compiled shadml shaders. It provides:

- **Binding reflection**: `BindingReflection`, `BindGroupReflection`, `EntryReflection` structs with group/index, address space, type, and push-constant sizes
- **GPU structs**: `#[repr(C)]` structs with `bytemuck` derives, matching WGSL struct layouts (size, alignment, field offsets)
- **Pipeline helpers**: `create_render_pipeline()` and `create_compute_pipeline()` methods that build wgpu pipeline layouts; per-render-block helpers like `create_{name}_render_pipeline_layout()` and `create_{name}_render_bind_group_layout_N()`
- **Embedded WGSL**: Shader source embedded as string constants (debug or minified)
- **Push constant helpers**: `ImmediatesGpu` type aliases, `PUSH_CONSTANT_SIZE` constants, and `set_immediates()` / `set_immediates_compute()` methods
- **ABI hashing**: `abi_hash` and `interface_hash` fields for detecting incompatible shader changes

#### Generated Code Example

For each entry point, the bindgen generates:
- A Rust module with GPU structs, bind group layouts, and pipeline creation methods
- Compile-time assertions for struct size and alignment
- Type aliases for immediate (push constant) data

### 17.9 Configuration (`shadml.toml`)

The bundler and bindgen are configured via `shadml.toml`:

```toml
[bundle]
source_roots = ["shaders"]
output_dir = "dist"
split_entry_points = true
features = []
preserve_comments = false

[[entry]]
file = "shaders/GradientTriangle.shadml"

[rust]
output = "src/generated/shaders.rs"
emit_rerun_if_changed = true
source_mode = "EmbeddedDebug"
type_map = "Plain"

[[rust.profile]]
name = "base"
features = []
```

**`[bundle]` options:**

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `source_roots` | `[String]` | `["."]` | Directories to search for imported modules |
| `output_dir` | `String` | `"dist"` | Directory for compiled WGSL output |
| `features` | `[String]` | `[]` | Feature flags to enable |
| `preserve_comments` | `bool` | `false` | Preserve source comments in WGSL output |
| `split_entry_points` | `bool` | `false` | Write one WGSL file per entry point |

**`[[entry]]` options:**

| Key | Type | Description |
|-----|------|-------------|
| `file` | `String` | Path to the shadml source file (required) |

**`[rust]` options:**

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `output` | `String` | required | Output path for generated Rust file |
| `emit_rerun_if_changed` | `bool` | `true` | Emit `cargo:rerun-if-changed` directives |
| `source_mode` | `String` | `"EmbeddedDebug"` | `"EmbeddedDebug"`, `"EmbeddedMinified"`, `"RuntimeBlob"`, or `"ServerFetch"` |
| `type_map` | `String` | `"Plain"` | `"Plain"`, `"Glam"`, or `"Nalgebra"` — maps Vec/Mat types to Rust crate types |

**`[[rust.profile]]` options:**

| Key | Type | Description |
|-----|------|-------------|
| `name` | `String` | Profile name (required) |
| `features` | `[String]` | Feature flags enabled for this profile |

## Appendix A: Complete Example

```shadml
-- A simple compute shader that scales a buffer of vec4s.

data ComputeInput = ComputeInput {
  @builtin(global_invocation_id) gid : Vec<3, U32>
}

@group(0)
  @binding(0) storage(read_write) input  : Array<Vec<4, F32>>
  @binding(1) storage(read_write) output : Array<Vec<4, F32>>

const SCALE : F32 = 2.0

scaleVec : Vec<4, F32> -> Vec<4, F32>
scaleVec v = vec4 (v.x * SCALE) (v.y * SCALE) (v.z * SCALE) v.w

main : ComputeInput -> ()
@compute @workgroup_size(64, 1, 1)
main input =
  let idx = toU32 input.gid.x
      v   = load (input[idx])
  in writeAt output idx (scaleVec v)
```

## Appendix B: Feature Summary

| Feature | Status |
|---------|--------|
| Hindley-Milner type inference | Implemented |
| Algebraic data types (records, enums, ADTs) | Implemented |
| Pattern matching with when-guards | Implemented |
| Native WGSL switch/case for integer patterns | Implemented |
| Multi-value or-patterns | Implemented |
| Named tail-recursive loops | Implemented |
| Traits with associated types and static dispatch | Implemented |
| Operator overloading (via associated types) | Implemented |
| Module system (file = module) | Implemented |
| Conditional compilation (`when cfg.x`) | Implemented |
| Bitfields with typed fields | Implemented |
| Pipeline operator (`\|>`) | Implemented |
| Lambda expressions (beta-reduced) | Implemented |
| Method-call syntax sugar | Implemented |
| Vec swizzle patterns | Implemented |
| Vec literal syntax (`[a, b, c]`) | Implemented |
| Matrix column access via swizzle | Implemented |
| Tuple desugaring | Implemented |
| Struct-based entry point I/O | Implemented |
| Render blocks (scoped vertex+fragment pipelines) | Implemented |
| Compute / vertex / fragment stages | Implemented |
| Texture and sampler bindings | Implemented |
| Binding arrays | Implemented |
| Push constants / immediates (`var<immediate>`) | Implemented |
| Dead code elimination | Implemented |
| Bundler (multi-file compilation, entry-point splitting) | Implemented |
| Bindgen (Rust code generation, GPU structs, pipeline helpers) | Implemented |
| LSP (diagnostics, completions, hover, goto-def, references) | Implemented |
| Code formatter | Implemented |
| WASM compilation target | Implemented |
| VS Code, Helix, Zed editor support | Implemented |
| Tree-sitter grammar | Implemented |
