Please implement a purely functional `shadml` that runs on WebGPU.
The implementation language is Rust, and the goal is a fast compiler/toolchain using arena allocation inspired by Oxc's design.
The compilation target is WGSL.

The language features should include:

- An ML-derived purely functional language
- Strongly influenced by Haskell
- Language features equivalent to Haskell, but without using special symbols such as `<$>`
- Static typing and HM type inference
- Function composition via `(.)`
- Everything, including operators, is a function, e.g. `((+))`
- Infix notation via backticks
- Functor, Applicative, Monad, and type classes
- ADTs and pattern matching
- Statically typed multidimensional representations via dependent types
- Based on a fallible syntax tree using CST
- Also implement a linter, formatter, and LSP
- A module system and bundler
- A web playground
