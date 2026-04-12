# Multi-Parameter Traits: Future Improvements

Created: 2026-04-12
Progress: 0/6 items

Future improvements for multi-parameter traits and constrained inference, now that the initial associated types migration is complete.

- [ ] Tighten diagnostics. The new errors work, but they can be more specific about the inferred constrained signature the user should write.
- [ ] Add formatter/LSP surface support for multi-constraint signatures if you want things like Add a b c, Show c => ... to be rendered cleanly everywhere.
- [ ] Decide whether ordinary trait methods should also use multi-parameter traits more broadly, not just operators.
- [ ] Revisit specialization/generic codegen. Right now the constrained-top-level rule keeps things manageable, but if shadml grows more generic libraries, specialization and template retention policy will matter more.
- [ ] Consider a builtin trait inventory instead of ad hoc builtin matching logic in semantic analysis. That would make the model cleaner and bring implementation closer to the language design.
- [ ] Update tree-sitter grammar if you want editor parsing/highlighting to fully reflect the new trait/impl head forms.
- [ ] Add more tests around ambiguity and partial application, since multi-parameter predicates make those cases more important.