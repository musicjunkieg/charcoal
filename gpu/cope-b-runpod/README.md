# Charcoal CoPE-B-A4B GPU service

vLLM-on-RunPod-Serverless harness for Charcoal's Stage-2 toxicity classifier.

## Files

- `Dockerfile` — image build. Bakes the model weights and `policy.txt` into the image.
- `handler.py` — RunPod Serverless worker entrypoint. Wraps vLLM's AsyncLLMEngine.
- `prompt.py` — Gemma chat template + POLICY/CONTENT body assembly.
- `policy.txt` — toxicity policy (versioned in git; see `docs/superpowers/specs/...` for authoring guidance).
- `runpod.yml` — RunPod endpoint config (manual via web console at create time).
- `requirements.txt` — Python runtime pins.
- `tests/test_prompt.py` — prompt assembly unit tests (CPU-only).
- `tests/test_handler.py` — handler unit tests with mocked vLLM.
- `tests/test_prefix_cache.py` — benchmark that prefix caching is firing.
- `tests/smoke_test.sh` — local end-to-end smoke against `vllm serve`.

## Local development

```bash
cd gpu/cope-b-runpod
python3 -m pip install -r requirements.txt    # heavy: pulls vllm
python3 -m pytest tests/                       # runs prompt + handler tests
./tests/smoke_test.sh                          # requires a CUDA GPU
```

On a CPU-only / Apple-Silicon box, `vllm` and `transformers` cannot be
installed/run. The handler unit tests stub `vllm`/`runpod`/`transformers`
so they still run; the tokenizer-dependent prompt tests `importorskip`
transformers and skip locally (they run in CI inside the vLLM image).

## Deploying

Images are built and published by `.github/workflows/build-cope-b-image.yml`
on pushes to `staging` and `main` when files under `gpu/cope-b-runpod/**`
change. The workflow publishes to `ghcr.io/musicjunkieg/charcoal-cope-b:<sha>`
with a manifest digest pinned in the resulting GitHub Actions summary.

RunPod endpoint is configured per `runpod.yml`. Updates to that file
require manual reconciliation in the RunPod web console (no IaC yet).

## Policy changes

Editing `policy.txt` requires an image rebuild. CI bumps `POLICY_VERSION`
to `policy-<short-sha>-<date>` automatically. Audit log captures
`policy_version` per classification so a change can be located post-hoc.

## Region

Endpoint runs in `us-west` to minimize round-trip from Railway production.
Verify before creating: `railway status` should show a us-west default region.
