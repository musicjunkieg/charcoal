# Charcoal: Predictive Threat Detection for Bluesky

## What is this?

Charcoal is a tool that identifies Bluesky accounts likely to engage with a
protected user's content in a toxic or bad-faith manner — and surfaces them
*before* that engagement happens. It does this by monitoring when a protected
user's posts get amplified (quoted or reposted), analyzing the audience that
amplification exposes them to, and scoring those accounts for both behavioral
hostility and topical proximity.

This MVP is the intelligence layer only. It produces a ranked threat list that
the user reviews manually. It does not automate any muting or blocking actions.

## Who is this for?

Me (Bryan). I'm a publicly visible person on Bluesky who posts about topics
that attract targeted hostility — fat liberation, queer identity, DEI work,
and tech community commentary. I am not a software developer, so this tool
needs to be something I can run and understand without deep technical knowledge.

Eventually, Charcoal will be a service that protects multiple users. For this
MVP, it only needs to work for a single Bluesky account.

## What should it do?

Charcoal has a setup step and two detection modes. The setup step runs first
and is a prerequisite for everything else.

### Step 0: Build My Topic Fingerprint (runs first, refreshes periodically)

Before Charcoal can measure anyone's topic overlap with me, it needs to
understand what I actually talk about. This step fetches my recent posting
history and extracts a topic fingerprint — a structured representation of
the subjects I'm active in and how much I post about each one.

This fingerprint should be shown to me as part of the output so I can
validate it. It's useful on its own — I want to see what the system thinks
my topic profile looks like, and I want to be able to say "yes, that's
accurate" or "no, you're missing X" or "that's weighted wrong."

The fingerprint should be refreshed periodically (weekly is probably fine for
an MVP) so it stays current as my posting patterns shift.

### Mode 1: Amplification Response (build first)

When someone quotes or reposts one of my posts, Charcoal should identify that
event, then fetch the follower list of the person who amplified my content.
Those followers just became part of my exposure surface — they can now see my
content in their timeline, framed by whatever the amplifier said about it.

For each account in that follower list, Charcoal should assess two things:

**Toxicity profile.** Based on that account's recent posting history, how
hostile is their language in general? This isn't about whether they've been
hostile to *me* specifically — it's about whether they have a pattern of toxic
engagement with anyone. Use an established toxicity classification approach
(like Google's Perspective API or a comparable model) to score their recent
posts.

**Topic overlap with me.** Does this account post about the same subjects I'm
publicly visible in? But here's the thing — I can't give you a clean list of
my topics, because I'm not entirely sure what my topic fingerprint looks like.
My posting spans fat liberation and body politics, queer and trans identity,
DEI and anti-racism, a cappella music and education, Atlassian/tech community
topics, AI/LLMs, and community governance and cybernetics — but those are just
the ones I can name off the top of my head. There are probably patterns I'm
not aware of.

So before Charcoal can measure anyone's topic overlap with me, it needs to
**build my topic fingerprint first** by analyzing my own recent posting
history. This is Step 0 of the pipeline (see below). The topic fingerprint
should be dynamic — it gets refreshed periodically so it reflects what I'm
actually talking about now, not what I was talking about six months ago.

Important nuance: topic overlap alone is NOT a threat signal. Someone who posts
supportively about fat liberation is an ally, not a threat. The combination of
high topic overlap AND high toxicity is the threat signal — it identifies people
who are active in my spaces AND are behaviorally hostile. Topic overlap without
toxicity is neutral. Toxicity without topic overlap is low-priority (they're
hostile but unlikely to encounter my content).

The output should be a ranked list showing: the account's handle, their
toxicity score, their topic overlap score, a combined threat score, and the
2-3 most toxic recent posts as evidence so I can evaluate whether the system
is making good calls.

### Mode 2: Background Sweep (build after Mode 1 works)

On a slower cadence (daily is fine), Charcoal should scan a broader pool:
followers of my followers who are active in my topic areas. This catches
accounts that haven't encountered me through amplification yet but are
topically proximate and behaviorally hostile — the "haven't collided yet but
probably will" pool.

This should use the same scoring approach as Mode 1, just pointed at a
different input set. The topic overlap filter is critical here because the
unfiltered followers-of-followers pool is enormous (potentially millions of
accounts). Filtering to accounts that are actually active in my topic areas
brings it down to a manageable size.

## How I want to see the results

For the MVP, the output format should be simple and readable. A local web page,
a generated markdown report, or even structured terminal output would all be
fine. I don't need a full dashboard yet. I need to be able to:

- See the ranked threat list with evidence (the toxic posts that drove the score)
- Understand why each account was flagged (which topics overlapped, how toxic)
- Quickly scan and decide "yes that's a real threat" or "no, false positive"

If the tool generates a file I can review, that works. If it serves a local
page I can open in my browser, that works too. I don't have a strong preference
on the output format — just make it easy to scan a list and evaluate the
reasoning behind each entry.

## Preferences and constraints

**Language.** Rust. I already maintain another Rust application and I value
the compiler's strictness as a quality backstop — I can't manually audit
every line of code, so having rustc and clippy catch problems is important.
Use idiomatic Rust patterns: the `?` operator for error propagation,
`anyhow::Result` for application errors, no `.unwrap()` calls.

**Package philosophy.** Prefer well-established open source crates over
writing custom implementations, especially for HTTP clients, API interaction,
data processing, serialization, and common utilities. If a well-maintained
crate exists for something, use it. Don't hand-roll what the ecosystem has
already solved.

**Development tooling.** This project uses Chainlink
(https://github.com/dollspace-gay/chainlink) for issue tracking, session
management, and coding guardrails. Run `chainlink init` at project setup.
Use Chainlink sessions to preserve context across work sessions, and break
large implementation tasks into Chainlink issues with subissues.

Use Deciduous (https://crates.io/crates/deciduous) for decision documentation.
When making architectural or implementation choices (which crate to use for
HTTP, how to structure the scoring pipeline, what data format for the topic
fingerprint, etc.), log each decision with Deciduous including what was chosen,
what alternatives existed, and why this choice was made.

**Platform.** Development happens on Fly.io sprites running Claude Code. The
finished tool should be runnable on a standard Linux box or Mac with minimal
setup (cargo build + environment variables for API keys).

**Bluesky API.** Use the AT Protocol / Bluesky API for fetching posts, follower
lists, and detecting quote/repost events. My Bluesky handle is [FILL IN YOUR
HANDLE]. For the MVP, polling on a reasonable interval is fine — I don't need
real-time WebSocket monitoring yet.

**Toxicity scoring.** I'd prefer to use Google's Perspective API if the free
tier rate limits are workable for the scale we're dealing with. If not, suggest
an alternative and explain the tradeoff. I don't want to self-host ML models
for the MVP — that's too much operational complexity.

**Data storage.** Keep it simple. A local SQLite database or whatever is most
natural for a local Rust application. No cloud databases for the MVP.

**Secrets.** I'll need API keys for the Perspective API and a Bluesky app
password (or OAuth token). Tell me what credentials I need to set up and I'll
provide them as environment variables.

## What does "done" look like?

The MVP is done when I can:

1. Run a command on my Mac.
2. It analyzes my recent posting history and shows me my topic fingerprint —
   a breakdown of what topics I post about and how much. I can look at this
   and say "yes, that's an accurate picture of what I talk about."
3. It detects that someone has recently quoted or reposted one of my posts.
4. It fetches that person's followers.
5. It scores those followers for toxicity and topic overlap with my fingerprint.
6. It shows me a ranked list of the highest-threat accounts with evidence.
7. I look at the list and can say "yes, these are people I'd want to block"
   or "no, these are false positives" — and that evaluation helps us tune the
   system.

A secondary milestone (Mode 2) is done when the same pipeline runs against
the followers-of-followers pool filtered by topic overlap on a daily sweep,
and produces a similarly useful ranked list.

## What this MVP is NOT

To be explicit about scope boundaries:

- No automated muting or blocking. I make all decisions manually for now.
- No multi-user support. This only protects my account.
- No web dashboard. A local output I can review is sufficient.
- No real-time monitoring. Polling on a reasonable interval is fine.
- No shared intelligence network. That's a future feature.
- No feed algorithm simulation. We're focused on direct amplification
  and second-degree network analysis.

## Reference material

There is an extensive architecture document from earlier design conversations
that describes the full Charcoal vision, including exposure graphs, shared
intelligence, Cloudflare Workers architecture, and a Durable Object-based
real-time monitoring system. That document describes where this project is
headed eventually, but this MVP intentionally does not implement most of it.
The architecture document can be found at [LINK TO charcoal-architecture-seed.md
IF YOU WANT TO INCLUDE IT] and should be treated as context for future
direction, not as requirements for this phase.

For production deployment, investigate Osprey
(https://github.com/roostorg/osprey), a high-performance real-time safety
rules engine originally built by Discord for combating spam, abuse, and
botting, and now open-sourced by ROOST. Osprey processes event streams through
human-written rules (in SML, a Starlark-like language) and outputs verdicts
and actions. It includes an investigation UI for querying past decisions and
writing new rules. Hailey's atproto-ruleset
(https://github.com/haileyok/atproto-ruleset) provides an existing mapping of
ATProto/Bluesky events to Osprey's event model, including a live Bluesky
labeler implementation. When Charcoal moves from MVP to production,
Charcoal's scoring algorithms (toxicity, topic overlap, threat scoring) could
be implemented as Osprey UDFs, giving us real-time event processing, an
investigation UI, and a proven rules framework — without building that
infrastructure from scratch. This is NOT in scope for the MVP.

## Open questions I'd like Claude Code to help me think through

These are decisions I haven't made yet. I'd like Claude Code to propose an
approach for each, explain the tradeoff, and let me confirm before
implementing.

1. **How should my topic fingerprint be extracted and presented?** I want to
   see what the system thinks I talk about, but I don't know what granularity
   is most useful. Should topics be broad categories ("politics," "technology")
   or specific ("fat liberation," "Atlassian Forge development")? Should I see
   a weighted list, a cluster visualization, or something else? Propose an
   approach and show me an example of what the output would look like.

2. **How many recent posts should we analyze per account for toxicity scoring?**
   More posts = more accurate profile but slower and more API calls. What's
   the right balance for an MVP?

3. **How do we detect quote/repost events for my account?** What's the simplest
   polling approach using the Bluesky API that would catch these within a
   reasonable window (30 min to a few hours is fine)?

4. **How should topic overlap work in practice?** I described the concept
   above. I'd like Claude Code to propose a concrete approach and explain
   why it chose that approach over alternatives.

5. **How should the combined threat score weight toxicity vs. topic overlap?**
   My instinct is that toxicity should be weighted more heavily — a very toxic
   account with moderate topic overlap is more concerning than a moderately
   toxic account with high topic overlap. But I'd like a proposed formula I
   can evaluate.
