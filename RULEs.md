## Context

You are working on a Rust/Cargo realtime-agnostic event engine.
Its purpose is to act as a connectable middleware layer between any
data source (CDC, REST, WebSocket) and any future web client.
It must be reusable as-is across multiple projects with zero coupling.

This codebase is your responsibility. You own its quality, structure,
and long-term maintainability from this prompt forward.

## Rule 0 — No more comment-banner separators

The era of this pattern is OVER:

// ─── Build event bus ────────────────────────────────
// ─── Build auth provider ─────────────────────────────
// ─── Start bus subscriber → router loop ──────────────

Every comment banner is an unambiguous signal that the surrounded
block MUST be extracted into its own named function or module.

If the extracted logic is reusable across ≥2 call sites → it becomes
a utility (lib-level). If it is specific to one context → it becomes
a private helper inside that module's file.

There are no exceptions. If you find yourself reaching for a banner,
extract the block instead.

## Structural constraints — enforced, non-negotiable

FILE LEVEL
  - Max 5 functions per file (pub or private, tests excluded)
  - Max 25 lines per function (blank lines and closing brace included)
  - One primary responsibility per file — name it accordingly

MODULE LEVEL
  - A module is a directory with mod.rs or a named .rs + directory
  - Modules must not be aware of each other's internals
  - Cross-module calls go through public interfaces only
  - Circular dependencies are a compile error AND a design error

FUNCTION LEVEL
  - Max 1 level of nesting inside a function body
  - Early return over nested if/else
  - No inline closures longer than 3 lines — name them
  - All fallible operations must use ? — no .unwrap() or .expect()
    outside of test code

## Refactor taxonomy

UTILITIES — go in src/utils/ or a dedicated src/{domain}/ crate
  Criteria: used by ≥2 unrelated modules, no internal state,
  pure or near-pure function signature.
  Examples: sequence ID generation, JWT validation helpers,
            event serialisation, fanout channel construction.

HELPERS — go in the same file, declared fn (not pub)
  Criteria: only ever called by the one function it supports,
  extracts a named sub-step to keep parent ≤25 lines.
  Examples: build_router(), bind_listener(), spawn_bus_loop().

NAMING CONVENTION
  Utilities  → verb_noun  (build_jwt_provider, create_fanout_pool)
  Helpers    → verb_noun  (bind_tcp, spawn_cdc_task)
  Do NOT use "util", "helper", "misc", or "common" as module names.
  Name modules after the domain concept they own.

## Zero-warning build — mandatory before any commit

Run the following and iterate until every check passes with 0 issues:

  cargo fmt --all
  cargo clippy --all-targets --all-features -- -D warnings
  cargo build --all-features
  cargo test --all-features

Clippy lints that must be respected at minimum:
  clippy::pedantic       (enabled globally in Cargo.toml)
  clippy::nursery        (enabled globally)
  clippy::unwrap_used    (zero unwrap outside #[cfg(test)])
  clippy::expect_used    (zero expect outside #[cfg(test)])
  clippy::panic          (zero panic! outside #[cfg(test)])

Add to Cargo.toml workspace section:
  [workspace.lints.clippy]
  pedantic  = "warn"
  nursery   = "warn"
  unwrap_used = "deny"
  expect_used = "deny"

## Inline documentation — every public item, no exceptions

/// Brief one-line description of what this function does.
///
/// # Purpose
/// What problem does this solve and why does it exist here.
///
/// # Arguments
/// * `config` — The server configuration bag. Must be validated upstream.
/// * `publisher` — Shared handle to the event bus publisher arc.
///
/// # Returns
/// `Ok(())` on clean shutdown. Propagates any infrastructure error.
///
/// # Errors
/// Returns [`ServerError::BusInit`] if the event bus cannot start.
/// Returns [`ServerError::BindFailed`] if the TCP port is already in use.
///
/// # Panics
/// Never panics. All fallible paths return Result.
///
/// # Example
/// ```rust
/// let cfg = ServerConfig::default();
/// run(cfg).await?;
/// ```
pub async fn run(config: ServerConfig) -> anyhow::Result<()> { ... }

Rules:
  - Every pub fn, pub struct, pub enum, pub trait gets a full block
  - Private fn gets at minimum a single /// line if non-obvious
  - No orphan //! crate-level docs — put them in lib.rs or main.rs
  - Examples must compile (use `cargo test --doc`)

## Module README specification

Each module directory MUST contain a README.md with these sections:

# {Module Name}

## Purpose
What this module is responsible for. One paragraph, no bullet points.
State what it owns, not what it does to other things.

## Importance
Why this module exists in the architecture. What breaks if removed.
What is the cost of getting it wrong.

## Entry points
List the public API surface — structs, traits, and async fns that
callers should know about. Explain each in one sentence.

## Relations
Diagram (ASCII or mermaid) showing upstream and downstream modules.
List explicit dependencies: what this module imports and who imports it.

## How it works
Narrative of the internal flow. No code, just plain English.
Walk through the happy path from input to output.

## Conditions of use
When should a developer reach for this module vs an alternative?
What invariants must hold before calling into it?
What config keys or environment variables affect its behaviour?

## Failure modes
What can go wrong, what error types it surfaces, and how callers
should handle them.

## Changelog
Brief notes on breaking interface changes. Keep last 3 entries.

## Reusability contract

This engine is a middleware layer. It has no opinion on:
  - What database produces events
  - What serialisation format flows on the bus
  - What authentication scheme the caller uses
  - What frontend framework consumes the WebSocket

Every module must be designed against a TRAIT, not a concrete type.
No module may import a sibling module's concrete struct directly —
only its trait bound.

New adapters (databases, auth schemes, transports) must be addable
by implementing a trait and registering in the producer/auth registry.
Zero changes to existing files.

Before every refactor, ask:
  "Could another project drop this module in with zero changes?"
  If yes → it is a utility. Keep it pure.
  If no  → document exactly what makes it context-specific.

≤ 5 fn / file ≤ 25
lines / fn 
0 unwrap outside tests 
0 clippy warnings 
rustdoc on every pub item README.md per module traits over concrete types utilities vs helpers taxonomy 
no comment banners → extract cargo test --doc must pass