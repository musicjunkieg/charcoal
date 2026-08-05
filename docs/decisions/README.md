# Architecture Decision Records

This directory holds ADRs (Architecture Decision Records) for {{PROJECT_NAME}}.
ADRs are short documents capturing architecturally significant decisions,
their context, and their consequences.

## When to write an ADR

Write one when you make a decision that:

- Will be hard or expensive to reverse later
- Future-you (or a teammate) will look at the code and wonder "why on earth?"
- Has trade-offs that are worth recording
- Sets a precedent for how similar decisions get made

Examples: choosing a database, picking an authentication strategy, deciding
on a deployment platform, settling on a code organization pattern.

Don't write one for:

- Style preferences (use a linter config instead)
- Trivial implementation choices
- Things that change every week

## How to write an ADR

1. Copy the template: `cp 0001-record-architecture-decisions.md NNNN-short-slug.md`
2. Increment NNNN to the next available number
3. Fill in Context, Decision, Consequences
4. Commit it as part of the change that introduces the decision
5. Optionally: log it in deciduous too (`/decision` slash command)

## Status lifecycle

- **Proposed** — being discussed, not yet committed to
- **Accepted** — in effect now
- **Deprecated** — no longer applies, but kept for historical context
- **Superseded by NNNN** — replaced by a later ADR; link to it

When superseding, leave the old ADR's content intact and just change the
status. ADRs are append-only history.

## Format

Based on Michael Nygard's original format:
<https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions>

## Index

<!-- Update this list as you add ADRs -->

- [0001 — Record architecture decisions](0001-record-architecture-decisions.md)
