# AGENTS.md

Guidance for AI agents working in this repository.

## Product

`ncli` is a Rust CLI for defining workflows that chain other CLIs. A workflow works like a shell alias with more power: one named command runs several external CLI invocations, so a multi-step routine becomes a single command.

Keep this purpose in mind for every change. `ncli` orchestrates other tools. It does not reimplement what those tools already do.

## Core Principles

These are requirements, not preferences.

1. **Correct, not workaround.** Fix the root cause. Do not special-case symptoms, swallow errors, add retries to hide bugs, or patch around a broken abstraction. If the right fix needs a larger change, make that change or stop and explain why.
2. **Extensible by design.** Leave room for the next case without rewriting what exists. Before adding a branch for a specific CLI, step, or option, ask whether it belongs in the model as data or as a type.
3. **DRY.** Each piece of knowledge has one home. Do not duplicate logic, parsing, validation, or constants between commands.
4. **Minimal.** Write the least code that solves the problem correctly and stays extensible. No speculative features, unused parameters, dead code, or indirection without a current use.

When these pull against each other, correctness comes first, then extensibility, then minimalism.

## Code Style

- Idiomatic Rust. Let the type system carry meaning: typed values over strings, error types over messages. Prefer the established crates and idioms already in use over hand-rolled equivalents.
- **No comments by default.** Add one only when the code cannot explain itself: a non-obvious reason, a constraint, or a subtle edge case. clap `///` help text is user-facing, not a comment.
- No file header comments or module doc comments.
- Clear names do the documenting. Match the conventions already in the codebase.

## Design Guidance

- Keep the workflow definition (what to run) separate from execution (how to run it).
- Run external commands through the one shared execution path in `process.rs`, passing arguments as lists rather than shell strings.
- A failing step must never look like success: propagate failures with meaningful exit codes and messages.
- Validate user input and configuration up front with clear errors, not deep inside execution.

## Working in This Repo

- Read the surrounding code before changing it. Reuse existing abstractions before adding new ones.
- Keep changes scoped to the task.
- Add or update tests alongside behavior changes. Test behavior, not implementation details.
- Before reporting a change as done, run the checks CI runs (see `.github/workflows/`) and report failures honestly.
- Do not commit or push unless asked. Commit messages are one short sentence.
