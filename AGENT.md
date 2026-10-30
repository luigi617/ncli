# AGENT.md

Guidance for AI agents working in this repository.

## Product

`ncli` is a Python CLI for defining workflows that chain other CLIs. A workflow works like a shell alias with more power: one named command runs an ordered sequence of external CLI invocations, so a multi-step routine becomes a single command.

Keep this purpose in mind for every change. `ncli` orchestrates other tools. It does not reimplement what those tools already do.

## Core Principles

These are requirements, not preferences.

1. **Correct, not workaround.** Fix the root cause. Do not special-case symptoms, swallow errors, add retries to hide bugs, or patch around a broken abstraction. If the right fix needs a larger change, make that change or stop and explain why.
2. **Extensible by design.** Every decision should leave room for the next case without rewriting what exists. Prefer a small, general mechanism (a registry, an interface, a data-driven definition) over hardcoded branches for specific CLIs, steps, or options. Before adding an `if` for a new case, ask whether the case belongs in the model as data or as a pluggable type.
3. **DRY.** Each piece of knowledge has one home. If you are about to duplicate logic, parsing, validation, or constants, extract it. Do not copy and paste code between commands or step types.
4. **Minimal.** Write the least code that solves the problem correctly and stays extensible. No speculative features, unused parameters, dead code, or layers of indirection that have no current use.

When these pull against each other, correctness comes first, then extensibility, then minimalism.

## Code Style

- Python. Follow PEP 8 and idiomatic modern Python (type hints, `pathlib`, dataclasses where they fit, f-strings).
- **No comments by default.** Add one only when the code cannot explain itself: a non-obvious reason, a constraint, or a subtle edge case. Never write comments that restate what the code does.
- **No file header comments or module docstrings** at the top of files.
- Clear names do the documenting. Prefer small, well-named functions over commented blocks.
- Match the conventions already in the codebase: naming, structure, error handling, and idioms.

## Design Guidance

- Keep the workflow definition (what to run) separate from execution (how to run it). New step types, sources of workflow definitions, or output modes should plug in without changes to the core.
- Run external commands through one shared execution path. Do not call `subprocess` ad hoc from different places.
- Pass arguments to external CLIs as argument lists, not interpolated shell strings, unless shell semantics are explicitly part of the feature.
- Propagate failures with meaningful exit codes and error messages. A failing step must never look like success.
- Validate user-defined workflow configuration at load time with clear errors, not deep inside execution.

## Working in This Repo

- Read the surrounding code before changing it. Reuse existing abstractions before adding new ones.
- Keep changes scoped to the task. Do not refactor unrelated code in the same change.
- Add or update tests alongside behavior changes. Tests should cover behavior, not implementation details.
- Run the tests and linters before reporting a change as done, and report failures honestly.
- Do not commit or push unless asked.
- Commit messages are one short, concise sentence.
