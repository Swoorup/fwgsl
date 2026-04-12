---
name: plan
description: Create or update a design plan in the project's plan/ directory. Use this when starting a non-trivial feature, bugfix, or architectural change that needs a plan before implementation.
argument-hint: "[topic/slug]"
allowed-tools: Read Write Edit Glob Grep Bash(jj *)
---

Create or update a plan in the project's `plan/` directory.

The argument `$ARGUMENTS` specifies the plan location, e.g. `ide/hover-improvements` or `standalone-topic`.
If no argument is given, read `plan/README.md` to understand the current plan landscape, then ask the user what to plan.

## Directory layout

```
plan/
  README.md              # Index — single source of truth for status
  associated-types/      # Topic subdirectories (2+ related plans)
  lowering/
  ide/
  prelude/
  standalone-topic.md    # One-off plans live at top level
```

Current topic subdirectories: `associated-types/`, `lowering/`, `ide/`, `prelude/`.

## Rules

### Where to place the plan

1. If the topic matches an existing subdirectory, place the plan file there:
   `plan/{subdirectory}/{slug}.md`
2. If it would be the first plan in a new topic area, place it at the top level:
   `plan/{slug}.md`
   Only create a new subdirectory when a second related plan appears.
3. Slug = short hyphenated name, no date prefix (dates come from `jj log`).

### Plan file structure

Every plan file must follow this structure:

```markdown
# <Title>

Created: <YYYY-MM-DD>
Progress: <done>/<total> <units>

<Summary paragraph>

## <Section>
- [x] Completed item
- [ ] Remaining item
```

Key rules:
- **Created** line: the date the plan was first written (never changes)
- **Progress** line: tracks completion — use `N/M phases`, `N/M items`, `N/M issues`, or `Completed`
- **Checkboxes** (`[x]`/`[ ]`): every concrete task, phase, or issue in the body uses these
- **No `status:` header** — status lives only in `plan/README.md` (single source of truth)
- **No date in filename** — the Created line inside the file is sufficient

### Updating the README index

After creating or completing a plan, update `plan/README.md`:

- **New plan**: Add under `## Active` with format:
  `- [path/without-ext](path.md) — one-line description`
  Group subdirectory plans together, then standalone stubs.
- **Completed plan**: Update `Progress:` line to `Progress: Completed`, then move the entry from `## Active` to `## Completed` in the README.

### When a plan is fully done

1. Mark all checkboxes `[x]` in the plan file
2. Set `Progress: Completed`
3. Move the README entry from Active to Completed

## Template

Use the template at `${CLAUDE_SKILL_DIR}/template.md` as the starting point for new plan files. Copy it, fill in the placeholders, and write it to the determined path.