# Charcoal CoPE-B-A4B GPU service

vLLM-on-RunPod-Serverless harness for the Stage-2 toxicity classifier.
See `docs/superpowers/specs/2026-06-05-cope-b-self-hosted-design.md` for design.

Files (filled in by Chunk 3):
- `Dockerfile` — image build
- `handler.py` — RunPod worker entrypoint
- `prompt.py` — Gemma chat template + POLICY/CONTENT assembly
- `policy.txt` — toxicity policy (versioned per Bryan; **not** silently
  derivable from CoPE-A's hosted labeler)
- `runpod.yml` — endpoint config
- `tests/` — handler unit tests + smoke script
