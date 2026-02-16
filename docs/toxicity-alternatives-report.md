# Toxicity Scoring Alternatives: Research Report

**Charcoal Issue #11** | February 9, 2026

Perspective API is sunsetting December 31, 2026 with no migration support.
Charcoal's `ToxicityScorer` trait already abstracts the scoring backend,
so swapping implementations is straightforward. This report evaluates
replacements.

---

## Executive Summary

**Recommended primary replacement: Detoxify's `unbiased-toxic-roberta` model
via ONNX Runtime in Rust.**

This model returns continuous 0-1 scores across 7 toxicity categories
(matching Charcoal's existing interface), was specifically trained to reduce
bias around identity mentions (critical for Bryan's topic areas), runs on
CPU with no API dependency, and has a pre-exported ONNX model ready for the
`ort` Rust crate. It's Apache 2.0 licensed.

**Recommended backup/complement: OpenAI Moderation API** as a zero-cost
cloud fallback, and **CoPE by Zentropi** for policy-steerable classification
of edge cases.

---

## Table of Contents

1. [What We Need](#what-we-need)
2. [Tools Evaluated](#tools-evaluated)
3. [Detailed Assessments](#detailed-assessments)
4. [Comparison Matrix](#comparison-matrix)
5. [Rust Integration Paths](#rust-integration-paths)
6. [Recommendations](#recommendations)
7. [Migration Plan](#migration-plan)

---

## What We Need

Charcoal's toxicity scorer must:

- **Return continuous scores (0.0-1.0)** -- the threat scoring formula uses
  weighted multiplication, not binary pass/fail
- **Detect toxicity relevant to social media** -- short posts, sarcasm,
  quote-post hostility, dog-whistling
- **Minimize identity-term bias** -- Bryan posts about fat liberation, queer
  identity, trans rights, and DEI. Models that flag these topics as toxic
  produce false positives that poison the threat scoring pipeline
- **Run without a GPU** -- Charcoal is a CLI tool on modest hardware
- **Integrate with Rust** -- ideally without adding Python as a dependency
- **Have no sunset risk** -- the whole point of this migration

Nice to have:
- Multi-category output (toxicity, insult, threat, identity_attack, etc.)
- Batch processing support for the amplification pipeline
- Small model footprint for eventual cloud deployment

---

## Tools Evaluated

### Requested by User (6)

| # | Tool | Verdict |
|---|------|---------|
| 1 | CoPE by Zentropi | Strong candidate (policy-steerable), but binary output |
| 2 | Detoxify by Unitary AI | **Top recommendation** |
| 3 | gpt-oss-safeguard by OpenAI | Interesting but too heavy for MVP |
| 4 | OSmod by Jigsaw | **Not viable** -- depends on Perspective API |
| 5 | Perspective API by Jigsaw | Confirmed sunsetting Dec 31, 2026 |
| 6 | toxic-prompt-roberta by Intel | Wrong domain (LLM prompts, not social media) |

### Found During Additional Research (10+)

| Tool | Category |
|------|----------|
| OpenAI Moderation API | Cloud API (free) |
| Google Cloud Natural Language API | Cloud API (paid) |
| Azure AI Content Safety | Cloud API (paid) |
| AWS Comprehend Toxicity | Cloud API (paid) |
| MiniLMv2-toxic-jigsaw-onnx | Local model (tiny, ONNX-ready) |
| IBM Granite Guardian HAP | Local model (tiny, Apache 2.0) |
| cardiffnlp/twitter-roberta-base-hate | Local model (social-media trained) |
| facebook/roberta-hate-speech-dynabench | Local model (adversarial-trained) |
| s-nlp/roberta_toxicity_classifier | Local model (Jigsaw-trained) |
| Hive Moderation API | Cloud API (paid) |

---

## Detailed Assessments

### 1. Detoxify / unbiased-toxic-roberta (Unitary AI)

**Verdict: TOP RECOMMENDATION**

A Python library providing pre-trained toxicity classifiers built on
HuggingFace Transformers, trained on all three Jigsaw/Kaggle toxic comment
challenges.

**The `unbiased` variant is the key model.** It uses RoBERTa-base (125M
params) and was specifically trained to minimize bias around identity
mentions -- the exact problem Charcoal faces with Bryan's posting topics.

| Attribute | Value |
|-----------|-------|
| Architecture | RoBERTa-base (125M params) |
| Output | Continuous 0-1 scores per category |
| Categories | toxicity, severe_toxicity, obscene, threat, insult, identity_attack, sexual_explicit |
| Training data | Jigsaw Unintended Bias challenge (Civil Comments) |
| AUC | 93.74% (unbiased), 98.64% (original/BERT) |
| License | Apache 2.0 |
| Model size | ~500 MB (PyTorch), ~125 MB (ONNX quantized) |
| GPU required | No -- runs on CPU |
| ONNX export | Pre-exported at `protectai/unbiased-toxic-roberta-onnx` |
| Maintenance | Active (last commit Jul 2025, ModernBERT config added Jan 2025) |

**Why it's the best fit:**

- **Continuous scores** map directly to Charcoal's existing `ToxicityScorer`
  trait return type -- no adapter needed
- **7 category scores** match what we already get from Perspective API
  (toxicity, severe_toxicity, insult, threat, identity_attack, obscene) plus
  sexual_explicit as a bonus
- **Identity-bias mitigation** was the explicit training objective of the
  `unbiased` model -- trained on content with demographic annotations to
  reduce false positives on identity-related text
- **Pre-exported ONNX model** at `protectai/unbiased-toxic-roberta-onnx`
  means zero ONNX conversion work -- just download and load with `ort`
- **No API dependency** -- runs entirely locally, no rate limits, no sunset
  risk, no network latency
- **Jigsaw training data** is the same dataset family that Perspective API
  was trained on, so scoring behavior will be broadly similar

**Weaknesses:**

- Training data is from 2017-2019 Wikipedia/Civil Comments -- may miss
  newer harassment patterns and social media slang
- No continuous model updates (unlike Perspective which was retrained)
- ~500 MB RAM footprint (manageable but not trivial on small VMs)
- CPU inference is ~50-200ms per text (slower per-request than Perspective
  API's cloud GPUs, but no network overhead)

---

### 2. CoPE by Zentropi AI

**Verdict: STRONG COMPLEMENT (not a drop-in replacement)**

A 9B parameter model (based on Gemma-2-9B with LoRA) that evaluates content
against custom policies written in plain English. The "policy steerability"
is its killer feature.

| Attribute | Value |
|-----------|-------|
| Architecture | Gemma-2-9B + LoRA (9B params) |
| Output | Binary only (0 or 1) |
| F1 scores | 90% toxic speech, 91% hate speech, 73% harassment |
| License | Zentropi OpenRAIL-M (permissive with use restrictions) |
| Model size | ~18 GB (full), ~5.7 GB (quantized) |
| GPU required | Yes for self-hosting; hosted API available |
| API | `https://api.zentropi.ai/v1/label` (free tier) |
| Paper | arxiv.org/html/2512.18027v1 (Dec 2024) |

**Why it's interesting for Charcoal:**

- You can write a policy like "Flag content that mocks, ridicules, or
  attacks people based on body size, gender identity, or sexual orientation"
  and the model evaluates against *that specific policy*
- 90% F1 on toxic speech vs GPT-4o's 75%
- Hosted API means no GPU needed for MVP
- Could be used as a *second-pass* scorer for accounts that land near
  threat-tier boundaries

**Why it's not the primary replacement:**

- **Binary output (0/1)** -- Charcoal's scoring formula needs continuous
  values (0.0-1.0). Would need multiple labelers at different strictness
  levels to approximate a scale, adding latency and complexity
- **73% F1 on harassment** -- weaker on the exact signal Charcoal cares
  about most
- **9B params requires GPU for self-hosting** -- or depends on Zentropi's
  hosted API (reintroducing external dependency)
- **Relatively new** -- paper Dec 2024, blog Jan 2026, limited external
  validation

---

### 3. gpt-oss-safeguard by OpenAI

**Verdict: INTERESTING BUT TOO HEAVY**

Open-weight reasoning model (MoE architecture) that follows custom safety
policies with chain-of-thought reasoning. Apache 2.0 licensed.

| Attribute | Value |
|-----------|-------|
| Architecture | MoE Transformer (20B total / 3.6B active params) |
| Output | Binary classification + reasoning chain |
| F1 scores | 82.9% OpenAI Mod, 79.9% ToxicChat |
| License | Apache 2.0 |
| Model size | ~12-15 GB (quantized 20B) |
| GPU required | 16 GB VRAM minimum for 20B |
| Local | Yes (Ollama, vLLM, llama.cpp) |
| Cloud | Groq, OpenRouter |

**Strengths:**
- Custom policies with reasoning (similar to CoPE but with explanations)
- Apache 2.0 is the most permissive license
- Available on Groq/OpenRouter for cloud inference
- 20B and 120B perform nearly identically (go with 20B)

**Why it's not right for Charcoal's MVP:**
- **Binary output** -- same problem as CoPE
- **Requires 16 GB VRAM GPU** for self-hosting
- **Slow** -- chain-of-thought reasoning adds significant latency per
  classification, bad for batch processing hundreds of posts
- **Research preview** -- only 2 commits on GitHub, no published roadmap
- OpenAI's own evaluation notes that "specialized classifiers trained on
  labeled examples can outperform gpt-oss-safeguard on narrow tasks"

**Future potential:** Could be a powerful second-pass tool for
high-priority accounts once Charcoal has GPU infrastructure.

---

### 4. OSmod by Jigsaw (ConversationAI Moderator)

**Verdict: NOT VIABLE**

A full-stack Node.js/React moderation application that depends entirely
on Perspective API for its scoring. It will stop working when Perspective
API sunsets.

- Effectively unmaintained (last human commit: 2023, only Dependabot bumps)
- Solves a different problem (moderating incoming comments on your platform)
- Not a library -- it's a full application with MySQL, Redis, etc.
- The Discourse plugin is archived; the Reddit plugin is stale
- 30 open issues with no triage

**Only takeaway:** The pluggable scorer pattern OSmod uses validates
Charcoal's `ToxicityScorer` trait architecture.

---

### 5. Perspective API by Jigsaw

**Verdict: SUNSETTING -- CONFIRMED**

The official perspectiveapi.com homepage now shows:

> "Important Notice: Perspective API is sunsetting and service is
> officially ending after 2026."

| Milestone | Date |
|-----------|------|
| Service shutdown | December 31, 2026 |
| Usage/quota requests accepted until | February 2026 |
| No extensions available | Confirmed |
| Migration support | None offered |

**Current specs for reference:**
- Free, 1 QPS default rate limit
- Categories: TOXICITY, SEVERE_TOXICITY, IDENTITY_ATTACK, INSULT,
  PROFANITY, THREAT (+ others)
- Returns 0.0-1.0 probability scores

Charcoal can continue using Perspective until December 2026, but the
replacement should be implemented well before then.

---

### 6. toxic-prompt-roberta by Intel

**Verdict: WRONG DOMAIN**

RoBERTa-base (125M params) fine-tuned on Jigsaw + ToxicChat data,
designed as an LLM guardrail -- detecting toxic *prompts* sent to
chat AI, not social media content.

| Attribute | Value |
|-----------|-------|
| Architecture | RoBERTa-base (125M params) |
| Output | Binary (toxic/not-toxic) |
| F1 | 0.79 on ToxicChat (better than LlamaGuard) |
| License | MIT |
| Model size | ~475 MB (F32), ~120 MB (ONNX INT8) |

**Why it's not right:**
- **Designed for LLM prompts, not social media posts** -- different
  linguistic patterns
- **Binary output only** -- no continuous scores
- **Lowest AUROC (0.718) on LGBTQ+ content** -- the worst-performing
  demographic subgroup is exactly the content area Bryan posts about
- **Self-described as proof-of-concept** needing "more testing"
- **25% false negative rate** could miss real threats

The unbiased-toxic-roberta model from Detoxify/Unitary is strictly
better for Charcoal's use case: same architecture, multi-label output,
identity-bias mitigation, and trained on comment data closer to social
media.

---

### 7. OpenAI Moderation API

**Verdict: BEST CLOUD BACKUP**

Free moderation API from OpenAI. 13 harm categories, built on GPT-4o,
supports text and images.

| Attribute | Value |
|-----------|-------|
| Price | Free |
| Categories | harassment, hate, self-harm, sexual, violence, illicit (with subcategories) |
| Accuracy | ~95% overall |
| Languages | 40+ |
| Rate limits | More generous than Perspective's 1 QPS |
| Model | omni-moderation-latest |

**Strengths:**
- Free -- no cost at any volume
- Higher accuracy than Perspective (95% vs ~92%)
- Multimodal (text + images)
- Easy HTTP integration via `reqwest`

**Weaknesses:**
- Categories don't map 1:1 to Perspective/Detoxify
- External dependency (reintroduces API risk, though OpenAI is unlikely
  to sunset their moderation endpoint)
- Requires an OpenAI API key

**Role in Charcoal:** Implement as a second `ToxicityScorer` variant
that can serve as fallback if local inference isn't available or for
cross-validation.

---

### 8. Notable Additional Findings

**MiniLMv2-toxic-jigsaw-onnx (Minuva)**
- Distilled from toxic-bert, only ~22M params, ships as quantized ONNX
- ROC-AUC 0.9813, Apache 2.0
- Could be the lightest-weight option if model size is critical for
  deployment (e.g., Cloudflare Workers)

**IBM Granite Guardian HAP-38m / HAP-125m**
- Apache 2.0, IBM-backed, tiny models (38M / 125M params)
- Detect Hate, Abuse, Profanity
- Could run alongside Charcoal with minimal overhead

**Google Cloud Natural Language API**
- 16 safety categories, PaLM 2 powered
- Closest interface to Perspective (same Google ecosystem)
- But paid, introducing cost where Perspective was free

**cardiffnlp/twitter-roberta-base-hate**
- RoBERTa fine-tuned on 58M tweets -- the only model specifically
  trained on social media content
- Worth evaluating alongside unbiased-toxic-roberta for social media
  linguistic patterns

---

## Comparison Matrix

| Model | Output Type | Categories | Params | Size (ONNX) | GPU? | License | Bias Mitigation | API Dep? |
|-------|-----------|------------|--------|-------------|------|---------|-----------------|----------|
| **unbiased-toxic-roberta** | Continuous (0-1) | 7 | 125M | ~125 MB | No | Apache 2.0 | Yes (explicit) | No |
| CoPE (Zentropi API) | Binary (0/1) | Custom | 9B | N/A | API | OpenRAIL-M | Policy-driven | Yes |
| gpt-oss-safeguard | Binary + reasoning | Custom | 20B | ~12 GB | Yes | Apache 2.0 | Policy-driven | No |
| OpenAI Moderation | Continuous (0-1) | 13 | N/A | N/A | N/A | Proprietary | Unknown | Yes |
| MiniLMv2-toxic-jigsaw | Continuous (0-1) | 6 | 22M | ~22 MB | No | Apache 2.0 | Unknown | No |
| Granite Guardian HAP | Continuous (0-1) | 3 (HAP) | 38M | ~40 MB | No | Apache 2.0 | Unknown | No |
| toxic-prompt-roberta | Binary (0/1) | 1 | 125M | ~120 MB | No | MIT | Low (0.718 LGBTQ+) | No |
| Perspective API | Continuous (0-1) | 6+ | N/A | N/A | N/A | N/A | Known issues | Yes (sunsetting) |

---

## Rust Integration Paths

All local models converge on the same Rust integration strategy:

### ONNX Runtime via `ort` crate (Recommended)

1. **Download** the ONNX model file (e.g., `protectai/unbiased-toxic-roberta-onnx`)
2. **Tokenize** input text using the `tokenizers` crate (HuggingFace's
   canonical Rust tokenizer -- the Python version wraps this)
3. **Run inference** through `ort` (Rust bindings to ONNX Runtime)
4. **Parse output** tensor into toxicity scores

The `ort` crate is production-grade (used by SurrealDB, Bloop, Google's
Magika). Supports CPU, CUDA, CoreML, DirectML backends.

### Alternative: `candle` (HuggingFace)

Pure Rust ML framework -- no C++ dependencies at all. Loads safetensors
directly. More manual work to set up the classification head but the
lightest dependency footprint.

### Alternative: `rust-bert`

Higher-level NLP library with built-in RoBERTa support. Uses `tch-rs`
(libtorch bindings) which adds a ~2 GB dependency. Has an `onnx` feature
flag that uses `ort` under the hood.

### For Cloud APIs

Use `reqwest` (already in Charcoal's dependency tree) to call HTTP
endpoints. Implement as a new struct behind the `ToxicityScorer` trait.

---

## Recommendations

### Primary: Detoxify unbiased-toxic-roberta via ONNX

Implement a new `OnnxToxicityScorer` struct that:
1. Loads `protectai/unbiased-toxic-roberta-onnx` at startup
2. Tokenizes post text with the `tokenizers` crate
3. Runs inference via the `ort` crate
4. Returns the same `ToxicityScore` struct the pipeline expects

This gives Charcoal:
- Zero API dependency (runs entirely locally)
- No rate limits (score as fast as CPU allows)
- Identity-bias mitigation (critical for Bryan's topic areas)
- Same output format as Perspective API
- ~125 MB model file (quantized ONNX)
- ~50-200ms per inference on CPU

### Backup: OpenAI Moderation API

Implement an `OpenAiModerationScorer` behind the same trait. Use this:
- As a fallback when local inference isn't available
- For cross-validation during development
- On resource-constrained deployments where ~500 MB RAM is too much

### Future: CoPE for edge cases

Once the primary scorer is stable, add CoPE as a second-pass tool for
accounts near threat-tier boundaries. Write Charcoal-specific policies:
- "Flag content that mocks people based on body size or appearance"
- "Flag concern-trolling about gender identity or sexual orientation"
- "Flag bad-faith engagement framed as 'just asking questions'"

This would require the Zentropi hosted API initially, with self-hosting
as an option if/when GPU infrastructure is available.

---

## Migration Plan

| Phase | What | When |
|-------|------|------|
| 1 | Add `ort` and `tokenizers` to Cargo.toml | Next session |
| 2 | Implement `OnnxToxicityScorer` behind existing trait | Next session |
| 3 | Add model download/caching logic | Next session |
| 4 | Test against Perspective API output for score calibration | Before switching |
| 5 | Make scorer configurable (env var to pick backend) | Before switching |
| 6 | Implement OpenAI Moderation scorer as backup | When convenient |
| 7 | Switch default from Perspective to ONNX | Well before Dec 2026 |
| 8 | Remove Perspective API code | After Dec 2026 |

The `ToxicityScorer` trait abstraction means phases 2-5 are the core
work. The rest of Charcoal's pipeline (threat scoring, report generation,
etc.) doesn't change at all.

---

## Sources

### Models
- [Detoxify (GitHub)](https://github.com/unitaryai/detoxify)
- [unbiased-toxic-roberta (HuggingFace)](https://huggingface.co/unitary/unbiased-toxic-roberta)
- [unbiased-toxic-roberta ONNX (HuggingFace)](https://huggingface.co/protectai/unbiased-toxic-roberta-onnx)
- [CoPE-A-9B (HuggingFace)](https://huggingface.co/zentropi-ai/cope-a-9b)
- [CoPE Paper (arXiv)](https://arxiv.org/html/2512.18027v1)
- [gpt-oss-safeguard (GitHub)](https://github.com/openai/gpt-oss-safeguard)
- [gpt-oss-safeguard Technical Report (OpenAI)](https://openai.com/index/gpt-oss-safeguard-technical-report/)
- [OSmod (GitHub)](https://github.com/conversationai/conversationai-moderator)
- [Intel/toxic-prompt-roberta (HuggingFace)](https://huggingface.co/Intel/toxic-prompt-roberta)
- [MiniLMv2-toxic-jigsaw-onnx (HuggingFace)](https://huggingface.co/minuva/MiniLMv2-toxic-jigsaw-onnx)
- [Granite Guardian HAP-38m (HuggingFace)](https://huggingface.co/ibm-granite/granite-guardian-hap-38m)
- [cardiffnlp/twitter-roberta-base-hate (HuggingFace)](https://huggingface.co/cardiffnlp/twitter-roberta-base-hate)

### APIs
- [Perspective API sunset announcement](https://perspectiveapi.com/)
- [OpenAI Moderation API](https://platform.openai.com/docs/guides/moderation)
- [Google Cloud Natural Language](https://cloud.google.com/natural-language/docs/moderating-text)
- [Azure AI Content Safety](https://learn.microsoft.com/en-us/azure/ai-services/content-safety/overview)
- [Zentropi API](https://zentropi.ai/)

### Rust Ecosystem
- [ort crate (ONNX Runtime)](https://github.com/pykeio/ort)
- [tokenizers crate (HuggingFace)](https://github.com/huggingface/tokenizers)
- [rust-bert](https://github.com/guillaume-be/rust-bert)
- [candle (HuggingFace Rust ML)](https://github.com/huggingface/candle)

### Research
- [Perspective API bias (ACL 2022)](https://aclanthology.org/2022.nlp4pi-1.2/)
- [Perspective API German bias (arXiv)](https://arxiv.org/html/2312.12651v3)
- [Detoxify vs Perspective comparison (PMC)](https://pmc.ncbi.nlm.nih.gov/articles/PMC11015521/)
