# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# Craft Anything

The rules below are loaded into every session (imports). They are mandatory for every change: follow them
without being asked, unless the person explicitly says otherwise for a specific task.

@AGENTS.md
@docs/pitfalls.md

The plan and assignments are `docs/ROADMAP.md` (open work: `cargo xtask pending`); what works today is
`docs/STATUS.md` (with `docs/tool-status.md`); where additions go is `docs/extending.md`. Private plans may
live in `plan/` (gitignored) when present.

## Commands

Rust 1.97+ (edition 2024). The desktop app needs the Linux packages listed in README ("Get started")
when built on Debian/Ubuntu. The crate map and layering are in AGENTS.md ("Workspace map"); the
pre-push checklist is its "Before you finish". This is the everyday loop:

```sh
cargo run --release -p craft-anything          # desktop app (the only crate that may use egui)
cargo run -p craft-anything-cli -- commands    # every tool as a CLI subcommand, by toolset
cargo run -p craft-anything-cli -- mcp         # serve the tools to an MCP client
cargo run -p craft-anything-cli -- doctor      # what a live run is missing (agents, engines)
```

Tests (a test's session goes through `craft_anything_engine::testing::session()`, never
`Session::default()` — see pitfalls):

```sh
cargo test -p <crate> <test_name>              # a single test
cargo test -p <crate>                          # one crate
cargo test -p <a> -p <b> --no-fail-fast        # several crates: one failure must not hide the next
cargo test --workspace                         # everything
```

`cargo xtask` (defined in `xtask/src/main.rs`): `ci` runs all checks in order; individually there are
`layers`, `locks`, `i18n-coverage`, `secrets [--history]`, `tools [--check]`, `schemas [--check]`,
`wasm`, `licenses`, `pending [--count]`, `version [set X.Y.Z]` and `ico`.
