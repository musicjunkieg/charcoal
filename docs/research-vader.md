# VADER Sentiment Analysis: Research Report for Charcoal

**February 16, 2026** | Research for Charcoal issue: sentiment as a toxicity complement

---

## The Problem This Research Addresses

Charcoal scores accounts for toxicity using Detoxify's `unbiased-toxic-roberta`
ONNX model. This model returns 0-1 scores for seven categories: toxicity,
severe_toxicity, obscene, identity_attack, insult, threat, and sexual_explicit.

The model has a known blind spot: **it flags strong language regardless of
intent.** The word "stupid" in "I am tired of writing this stupid essay" yields
a 99.70% toxicity score. Remove the word and the score drops to 0.05%.
([Source: Unitary AI](https://www.unitary.ai/articles/how-well-can-we-detoxify-comments-online))

This matters for Charcoal because the protected user's community uses strong,
affirming language. Consider these two posts:

- **"fuck yeah, fat liberation is beautiful"** -- ally, supporter, not a threat
- **"fat people are disgusting"** -- hostile, exactly the kind of account Charcoal
  should flag

Both will trigger elevated toxicity scores due to the presence of profanity
and identity-related terms. The toxicity model alone cannot distinguish between
them because it measures *what language is present*, not *how it feels*.

**Sentiment analysis measures the emotional valence** -- positive, negative,
or neutral. Combined with toxicity, it could help separate allies from threats.

This report evaluates VADER as one approach to adding that signal.

---

## Table of Contents

1. [What VADER Actually Does](#1-what-vader-actually-does)
2. [Rust Availability](#2-rust-availability)
3. [How VADER Compares to the ONNX Toxicity Model](#3-how-vader-compares-to-the-onnx-toxicity-model)
4. [Practical Value for Charcoal](#4-practical-value-for-charcoals-use-case)
5. [Limitations](#5-limitations)
6. [Alternative Sentiment Approaches](#6-alternative-sentiment-approaches)
7. [Recommendation](#7-recommendation)

---

## 1. What VADER Actually Does

VADER stands for **Valence Aware Dictionary and sEntiment Reasoner**. It was
created by C.J. Hutto and Eric Gilbert and published at ICWSM (the
International Conference on Web and Social Media) in 2014. It was designed
specifically for social media text.

### The plain-language version

VADER is a dictionary-based tool. It has a list of about 7,500 words and
phrases, and each one has a sentiment rating from humans -- a number that says
how positive or negative that word feels. "Love" is positive. "Hate" is
negative. "The" is neutral. When you feed VADER a sentence, it looks up each
word, applies some rules about how the words interact, and gives you a final
score.

It is **not** a machine learning model. It does not learn from data. It follows
a fixed set of rules written by the researchers, based on patterns they
observed in how people express sentiment on social media. This makes it fast,
predictable, and easy to understand -- but also rigid.

### The five rules

Beyond the dictionary lookup, VADER applies five grammatical/syntactical rules
that adjust sentiment intensity:

1. **Punctuation** -- Exclamation marks amplify intensity. "The food is
   great!!!" scores more positively than "The food is great." Each exclamation
   adds a small empirically-determined boost (approximately +0.292 per "!").

2. **Capitalization** -- ALL CAPS on a sentiment word increases intensity.
   "The food is GREAT" scores higher than "The food is great." This only
   applies when the rest of the text is not also capitalized.

3. **Intensifiers (degree modifiers)** -- Words like "extremely," "very,"
   "slightly," and "somewhat" modify the intensity of the sentiment word
   that follows. "Extremely good" > "very good" > "good" > "marginally good."

4. **Contrastive conjunctions** -- The word "but" signals a shift, and the
   sentiment after "but" dominates. "The food is great, but the service is
   terrible" leans negative.

5. **Negation** -- VADER checks the three words before a sentiment word for
   negation ("not," "isn't," "never," etc.). "The food is not great" flips
   the polarity. This catches about 90% of negation cases.

### Output format

VADER returns four scores for any text:

| Score | Meaning | Range |
|-------|---------|-------|
| `pos` | Proportion of text that is positive | 0.0 to 1.0 |
| `neg` | Proportion of text that is negative | 0.0 to 1.0 |
| `neu` | Proportion of text that is neutral | 0.0 to 1.0 |
| `compound` | Normalized overall sentiment | -1.0 to +1.0 |

The **compound score** is the most useful single number. The standard
classification thresholds are:
- Positive: compound >= 0.05
- Neutral: compound between -0.05 and 0.05
- Negative: compound <= -0.05

### Performance claims

The original paper reports that VADER outperforms individual human raters on
social media text, with F1 scores of 0.96 on tweets (compared to 0.84 for
human agreement). It processes text extremely fast -- about 0.3ms per sentence,
roughly 130x faster than transformer-based models.

### Sources for this section

- [Original VADER paper (Hutto & Gilbert, ICWSM 2014)](http://eegilbert.org/papers/icwsm14.vader.hutto.pdf)
- [VADER GitHub repository](https://github.com/cjhutto/vaderSentiment)
- [VADER sentiment analysis explained (Pio Calderon)](https://medium.com/@piocalderon/vader-sentiment-analysis-explained-f1c4f9101cd9)
- [QuantInsti VADER guide](https://blog.quantinsti.com/vader-sentiment/)

---

## 2. Rust Availability

There are two Rust implementations of VADER on crates.io/GitHub.

### vader_sentiment (original Rust port)

| Attribute | Value |
|-----------|-------|
| Crate name | `vader_sentiment` |
| Version | 0.1.1 |
| Repository | [github.com/ckw017/vader-sentiment-rust](https://github.com/ckw017/vader-sentiment-rust) |
| Last pushed | November 2022 |
| Stars | 51 |
| Open issues | 2 |
| Rust edition | 2015 (old) |
| License | MIT |
| Dependencies | `regex`, `maplit`, `lazy_static`, `unicase` |

This is the original Rust port. It works but uses the old Rust 2015 edition
and has not been updated in over three years. The dependency footprint is
minimal (no heavy crates). It is a direct translation of the Python VADER
library.

### vader-sentimental (faster fork)

| Attribute | Value |
|-----------|-------|
| Crate name | `vader-sentimental` |
| Version | 0.1.2 |
| Repository | [github.com/bosun-ai/vader-sentimental](https://github.com/bosun-ai/vader-sentimental) |
| Last pushed | February 2025 |
| Stars | 6 |
| Open issues | 0 |
| Rust edition | 2021 (modern) |
| License | MIT |
| Dependencies | `regex`, `lazy_static`, `unicase`, `clap`, `hashbrown` |
| Created by | Bosun AI (Timon Vonk), forked from ckw017's port |

This is a modernized, performance-optimized fork. It uses Rust 2021 edition,
`hashbrown` for faster hash maps, and includes benchmarks comparing its
performance against the original `vader_sentiment` crate. The `clap`
dependency is for a CLI example and would not affect library usage. It was
actively maintained as recently as February 2025.

### Assessment

**`vader-sentimental` is the better choice** if Charcoal were to add VADER.
It uses modern Rust, has recent activity, comes from an AI company (Bosun AI)
that likely uses it in their own products, and benchmarks itself against the
original.

Neither crate has a large user base. Both are thin wrappers around a
well-understood algorithm (the VADER lexicon and rules), so the implementation
risk is low -- the algorithm is deterministic and well-documented, and the
code can be audited quickly.

**Dependency cost:** Both crates add only lightweight dependencies (`regex`
and `lazy_static` are already in most Rust projects). No ML runtime, no model
files, no downloads. This is one of VADER's biggest advantages -- it is
essentially just a dictionary lookup with some arithmetic.

---

## 3. How VADER Compares to the ONNX Toxicity Model

This is the critical conceptual point: **VADER and Detoxify measure completely
different things.** They are not competitors -- they are measuring different
dimensions of the same text.

### What each tool measures

| Dimension | Detoxify (unbiased-toxic-roberta) | VADER |
|-----------|----------------------------------|-------|
| **What it detects** | Presence of toxic language patterns | Emotional valence (positive/negative/neutral) |
| **Output** | 7 toxicity category scores (0-1) | Compound sentiment score (-1 to +1) |
| **Technique** | Transformer neural network (125M parameters) | Lexicon lookup + 5 hand-coded rules |
| **Training** | Learned from ~2M labeled comments | No training -- fixed dictionary from human ratings |
| **Speed** | ~50-200ms per text (CPU) | <1ms per text |
| **Context understanding** | Moderate (sees whole sentence) | Low (word-by-word with local rules) |
| **Model size** | ~126 MB ONNX file | ~50 KB lexicon file (embedded in crate) |

### The key insight: they answer different questions

- **Detoxify asks:** "Does this text contain language patterns associated
  with toxicity?" -- It looks for words and patterns that, in training data,
  appeared in comments that human annotators labeled as toxic.

- **VADER asks:** "Is the overall feeling expressed in this text positive or
  negative?" -- It looks at the emotional valence of each word and how they
  combine.

### How they complement each other

These are genuinely orthogonal measurements. You can have:

| Toxicity | Sentiment | Example | Interpretation |
|----------|-----------|---------|----------------|
| High | Positive | "fuck yeah, fat liberation is beautiful" | Strong affirming language -- probably an ally |
| High | Negative | "fat people are disgusting" | Hostile language about the topic -- real threat |
| High | Neutral | "the word 'retarded' should be retired from casual use" | Discussing toxicity itself -- meta-commentary |
| Low | Negative | "I find their approach deeply misguided and harmful" | Civil but negative -- possibly concern-trolling |
| Low | Positive | "great post, totally agree" | Supportive, benign |
| Low | Neutral | "posted a photo of their lunch" | Irrelevant content |

The combination of **high toxicity + positive sentiment** is where VADER could
most directly help Charcoal. This pattern almost certainly indicates someone
using strong language in an affirming way -- exactly the false-positive
scenario described at the top of this report.

---

## 4. Practical Value for Charcoal's Use Case

### How the threat score currently works

Charcoal's threat formula is:

```
threat_score = (toxicity * 70) + (topic_overlap * 30)
```

With a gate: if `topic_overlap < 0.05`, the score is capped at 25 (the
account is hostile but not in the protected user's topic space, so they are
unlikely to engage).

The `toxicity` input is the average toxicity score across the account's
recent posts, as scored by the ONNX model.

### Where sentiment could intervene

Sentiment analysis could act as a **modifier on the toxicity input**, reducing
the effective toxicity score for accounts that consistently show positive
sentiment alongside their strong language.

#### Concrete walkthrough

**Account A: An ally who uses strong language**

Posts:
- "fuck yeah trans rights are human rights" -- toxicity: 0.85, sentiment: +0.75
- "goddamn this fat liberation thread is fire" -- toxicity: 0.72, sentiment: +0.82
- "hell yes, more of this please" -- toxicity: 0.60, sentiment: +0.88

Average toxicity: 0.72. Average sentiment: +0.82. Topic overlap: 0.35.

Without sentiment adjustment: `0.72 * 70 + 0.35 * 30 = 50.4 + 10.5 = 60.9` (High tier)

With a hypothetical sentiment adjustment (e.g., discount toxicity by 50% when
average sentiment is strongly positive): `0.36 * 70 + 0.35 * 30 = 25.2 + 10.5 = 35.7` (Watch tier)

The ally drops from High (which could trigger automated action in a future
version) to Watch (monitored but not flagged).

**Account B: A hostile actor**

Posts:
- "these fat acceptance people are delusional" -- toxicity: 0.82, sentiment: -0.67
- "another day another woke meltdown lmao" -- toxicity: 0.45, sentiment: -0.42
- "imagine thinking obesity is healthy" -- toxicity: 0.71, sentiment: -0.55

Average toxicity: 0.66. Average sentiment: -0.55. Topic overlap: 0.40.

Without sentiment adjustment: `0.66 * 70 + 0.40 * 30 = 46.2 + 12.0 = 58.2` (High tier)

With sentiment adjustment: negative sentiment does not reduce toxicity, so
the score stays the same or could even be slightly increased.

**The net effect:** Allies get downgraded. Hostile actors stay flagged. The
false positive rate drops for the exact population that matters most --
supportive community members who happen to use colorful language.

### How much false-positive reduction to expect

This is hard to quantify without testing against real data, but the underlying
research supports the thesis. The Surge AI blog post
["Holy $#!t: Are popular toxicity models simply profanity detectors?"](https://surgehq.ai/blog/are-popular-toxicity-models-simply-profanity-detectors)
found that toxicity models are heavily triggered by profanity regardless of
context, and that "the strongest profanities are often used in the most
positive, life-affirming ways." People's most enthusiastic supporters are
the ones most likely to be falsely flagged.

For Charcoal specifically, given that the protected user's community is one
where affirming, profanity-laced solidarity is common, the false positive
problem is likely more severe than average.

---

## 5. Limitations

VADER is a 2014 lexicon-based tool. It has real weaknesses, and they matter
for Charcoal's use case.

### Sarcasm and irony

VADER cannot detect sarcasm. "Oh, what a *wonderful* idea to harass people"
would score as positive because "wonderful" is a positive word. Sarcasm flips
meaning in a way that requires understanding intent, and VADER has no model
of intent.

**Impact for Charcoal:** Sarcastic hostility would be scored as positive
sentiment, potentially causing a false *negative* (reducing the threat score
for someone who is actually hostile). However, sarcastic users tend to also
produce non-sarcastic toxic content, so their average sentiment would likely
still lean negative across multiple posts.

### Context blindness

VADER processes words mostly independently. It does not understand that
"I was attacked for being fat" describes victimization, not aggression. The
word "attacked" gets scored negatively regardless of who is doing the
attacking.

**Impact for Charcoal:** Posts describing experiences of harassment (which
allies often share) might score as negative sentiment, partially undermining
the signal that was supposed to help identify them as allies.

### Lexicon staleness

The VADER lexicon was compiled in 2014. It does not include:
- Post-2014 slang ("bussin," "no cap," "slay")
- Bluesky-specific conventions
- Evolving reclaimed language
- New dog-whistles and coded hostility

**Impact for Charcoal:** Newer slang that allies use enthusiastically would be
scored as neutral (unknown words are ignored), not positive. Newer dog-whistles
used by hostile actors would also be missed. The bias here is toward
**underscoring both sides** rather than systematically favoring one over the
other.

### English only

VADER was designed for English text. It has minimal support for other
languages.

**Impact for Charcoal:** Minimal for the MVP, since the protected user's
community is primarily English-speaking. Would matter for future expansion
to other protected users.

### No understanding of identity reclamation

VADER does not understand reclaimed language. "Queer" has a positive meaning
in LGBTQ+ communities but a negative one when used as a slur. "Fat" is
neutral-to-positive in fat liberation spaces but negative in mainstream
usage. VADER's lexicon has fixed scores for these words and cannot adjust
based on community context.

**Impact for Charcoal:** This is the most significant limitation. The very
words that define the protected user's community have ambiguous valence.
VADER might score "fat liberation" neutrally or even negatively, when in
context it is a deeply positive phrase.

### Speed is a non-issue, accuracy is

VADER's speed advantage (sub-millisecond vs. 50-200ms for the ONNX model)
is irrelevant for Charcoal. The bottleneck is the Bluesky API rate limit
and network latency, not local text processing. Adding VADER would add
zero perceptible time to the scoring pipeline. The question is purely
about accuracy.

---

## 6. Alternative Sentiment Approaches

### Option A: VADER (lexicon-based)

- **Pros:** Tiny dependency (~50 KB), sub-millisecond speed, well-understood
  algorithm, two Rust crates available, no model download needed
- **Cons:** Lexicon from 2014, no context understanding, no sarcasm detection,
  no understanding of reclaimed language
- **Integration effort:** Very low -- add crate, call function, get compound
  score

### Option B: Sentiment ONNX model (transformer-based)

A transformer-based sentiment model (like DistilBERT fine-tuned on SST-2)
run through the same ONNX pipeline Charcoal already uses for toxicity.

- **Pros:** Much better context understanding than VADER, understands sentence
  structure, more accurate on complex social media text
- **Cons:** Adds a second ONNX model (~250-350 MB), doubles inference time,
  requires tokenizer management for a different model architecture
- **Integration effort:** Moderate -- Charcoal already has the ONNX pipeline
  (`ort` + `tokenizers`), so the infrastructure exists. Would need a second
  model download command, a second `Session`, and a second tokenizer.

The `rust-bert` crate provides ready-made sentiment analysis pipelines with
ONNX support via the `ort` crate. However, it brings a large dependency
tree and would be overkill when Charcoal already has its own ONNX infrastructure.

A simpler path: find a sentiment model on HuggingFace that is already exported
to ONNX (many exist, like `nlptown/bert-base-multilingual-uncased-sentiment`
or `cardiffnlp/twitter-roberta-base-sentiment-latest`), and load it the same
way Charcoal loads the toxicity model. The
[`cardiffnlp/twitter-roberta-base-sentiment-latest`](https://huggingface.co/cardiffnlp/twitter-roberta-base-sentiment-latest)
model is particularly interesting because it was trained on ~124M tweets --
making it well-suited for social media text.

### Option C: Better use of existing toxicity model categories

Charcoal already gets seven category scores from Detoxify. Instead of adding
a new tool, the *pattern* of category scores could be used to distinguish
allies from threats:

- **Ally pattern:** High `obscene` (profanity), low `identity_attack`, low
  `insult`, low `threat`. The person swears enthusiastically but is not
  targeting anyone.

- **Threat pattern:** High `identity_attack`, high `insult`, with or without
  high `obscene`. The person is attacking identity groups or directing insults
  at people.

- **Concern-troll pattern:** Low `obscene`, low `identity_attack`, moderate
  `toxicity`. The person avoids flagged language while still being hostile
  (VADER would not help here either).

This approach requires no new dependencies. It is a scoring formula change,
not an infrastructure change. The data is already available in
`ToxicityAttributes`.

**The specific implementation:** Instead of using the single `toxicity` score
as input to the threat formula, create a weighted combination that discounts
`obscene`/profanity and emphasizes `identity_attack`, `insult`, and `threat`:

```
adjusted_toxicity = (
    identity_attack * 0.35 +
    insult * 0.30 +
    threat * 0.20 +
    severe_toxicity * 0.10 +
    obscene * 0.05
)
```

This weights the categories that indicate targeted hostility much higher than
the category that just detects profanity. "fuck yeah, fat liberation" would
score high on `obscene` but low on `identity_attack` and `insult`, bringing
the adjusted toxicity down. "fat people are disgusting" would score high on
`identity_attack` and `insult`, keeping adjusted toxicity high.

### Option D: Hybrid approach (VADER + category weighting)

Use both: adjust the toxicity formula with better category weighting (Option C)
AND add VADER as a secondary signal. The category weighting handles the
mechanical false-positive problem (profanity != hostility), while VADER adds
a coarse emotional-direction signal.

This is the most thorough approach but also the most complex to calibrate. Two
new variables in the scoring formula means more tuning surface and more
potential for unintended interactions.

---

## 7. Recommendation

### Short answer: Start with Option C (category weighting), consider VADER later

### Reasoning

1. **The lowest-hanging fruit is already in your data.** Charcoal gets seven
   toxicity category scores but currently only uses the overall `toxicity`
   score. The single highest-impact change is to weight `identity_attack`,
   `insult`, and `threat` much higher than `obscene` when computing the input
   to the threat formula. This directly addresses the "profanity is not
   hostility" problem with zero new dependencies.

2. **VADER's biggest limitation hits Charcoal's exact domain.** The protected
   user's community uses reclaimed language ("fat," "queer") and
   profanity-as-affirmation ("fuck yeah"). VADER's fixed 2014 lexicon does
   not understand reclamation. It might score "fat liberation is beautiful"
   as neutral or negative because "fat" is in its dictionary as a negative
   word. This partially undermines the very reason to add it.

3. **A sentiment ONNX model would be better than VADER for accuracy**, but
   adds significant infrastructure weight (a second model, second tokenizer,
   more disk space, more RAM). This is worth doing eventually but is not the
   right first step.

4. **VADER is worth adding as a cheap secondary signal** once category
   weighting is implemented and tested. Its compound score provides
   directional information that categories alone do not capture. It costs
   almost nothing in terms of dependencies or performance. But it should
   complement the category weighting, not replace it.

### Suggested sequence

| Step | What | Complexity | Dependencies |
|------|------|-----------|--------------|
| 1 | Adjust `compute_threat_score` to accept category breakdown, weight `identity_attack`/`insult`/`threat` higher than `obscene` | Low | None -- data already available |
| 2 | Test adjusted scoring against known ally/threat accounts to calibrate weights | Low | Requires test data |
| 3 | (Optional) Add `vader-sentimental` crate as a supplementary signal | Low | One lightweight crate |
| 4 | If sentiment proves valuable, evaluate `cardiffnlp/twitter-roberta-base-sentiment-latest` as a more accurate ONNX-based sentiment scorer | Medium | Second ONNX model |

Step 1 alone will likely capture most of the false-positive reduction the user
is looking for. Steps 3 and 4 provide incremental improvements at increasing
cost.

### If VADER is added, how to integrate it

The compound score would work best as a **modifier** on the adjusted toxicity
score, not as a separate input to the threat formula:

```
// Pseudocode
let adjusted_tox = weighted_category_score(toxicity_result);
let sentiment = vader.polarity_scores(text).compound;

// If sentiment is positive, discount toxicity (ally signal)
// If sentiment is negative, leave toxicity unchanged or boost slightly
let sentiment_modifier = if sentiment > 0.3 {
    0.7  // 30% discount for clearly positive text
} else if sentiment < -0.3 {
    1.1  // 10% boost for clearly negative text
} else {
    1.0  // no change for neutral/ambiguous
};

let final_toxicity = (adjusted_tox * sentiment_modifier).clamp(0.0, 1.0);
```

Apply this per-post, then average across the account's posts. This way
sentiment modulates toxicity at the individual post level, where context
matters most, rather than as an account-wide average where the signal would
be diluted.

---

## Sources

### VADER
- [VADER paper: Hutto & Gilbert, ICWSM 2014](http://eegilbert.org/papers/icwsm14.vader.hutto.pdf)
- [VADER GitHub (Python original)](https://github.com/cjhutto/vaderSentiment)
- [vader_sentiment Rust crate](https://crates.io/crates/vader_sentiment) / [GitHub](https://github.com/ckw017/vader-sentiment-rust)
- [vader-sentimental Rust crate (faster fork)](https://github.com/bosun-ai/vader-sentimental)
- [VADER sentiment analysis explained (Medium)](https://medium.com/@piocalderon/vader-sentiment-analysis-explained-f1c4f9101cd9)
- [VADER guide (QuantInsti)](https://blog.quantinsti.com/vader-sentiment/)
- [VADER vs RoBERTa comparison (GitHub)](https://github.com/Soysamson/Comparing-VADER-to-RoBERTa-Models)

### Toxicity models and false positives
- [Detoxify / unbiased-toxic-roberta (HuggingFace)](https://huggingface.co/unitary/unbiased-toxic-roberta)
- [How well can we detoxify comments online? (Unitary AI)](https://www.unitary.ai/articles/how-well-can-we-detoxify-comments-online)
- [Holy $#!t: Are popular toxicity models simply profanity detectors? (Surge AI)](https://surgehq.ai/blog/are-popular-toxicity-models-simply-profanity-detectors)
- [Comparative analysis: RoBERTa vs VADER (Grenze)](https://thegrenze.com/pages/servej.php?fn=520.pdf&name=Comparative+Analysis+of+Sentiment+Analysis+Models:RoBERTa+vs.+VADER&id=2387&association=GRENZE&journal=GIJET&year=2024&volume=10&issue=1)

### Sentiment models for Rust
- [rust-bert (Rust NLP library)](https://github.com/guillaume-be/rust-bert)
- [cardiffnlp/twitter-roberta-base-sentiment-latest (HuggingFace)](https://huggingface.co/cardiffnlp/twitter-roberta-base-sentiment-latest)
- [Visualizing Public Opinion: VADER and DistilBERT (arXiv)](https://arxiv.org/html/2504.15448v2)

### Hybrid approaches
- [Sentiment analysis with VADER and Twitter-RoBERTa (Medium)](https://medium.com/@amanabdulla296/sentiment-analysis-with-vader-and-twitter-roberta-2ede7fb78909)
- [Sentiment and toxicity as auxiliary tasks in stance detection (MDPI)](https://www.mdpi.com/2079-9292/14/11/2126)
