# Charcoal CoPE-B-A4B GPU service

vLLM-on-RunPod-Serverless harness for Charcoal's Stage-2 toxicity classifier.

## Weights live on a network volume (not in the image)

The CoPE-B-A4B model is ~50 GB (Gemma-4 26B-A4B, 25B params). It is **not**
baked into the image — that produced a ~60 GB image that won't build on
standard CI runners and re-pulls on every cold start. Instead the weights live
on a **RunPod network volume** mounted at `/runpod-volume`, populated once and
reused across cold starts. `MODEL_PATH=/runpod-volume/cope-b-a4b` points vLLM at
them; the image stays small (just vLLM + app code).

## Files

- `Dockerfile` — image build (vLLM base + app code only; weights are volume-mounted).
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

Order of operations (one-time):

1. **Build + publish the image.** `.github/workflows/build-cope-b-image.yml`
   builds on pushes to `staging`/`main` (and, temporarily, the feature branch)
   touching `gpu/cope-b-runpod/**`, publishing
   `ghcr.io/musicjunkieg/charcoal-cope-b:<sha>` (and a slugified branch tag).
   The image is small now (no weights), so it builds on a stock runner.

2. **Create the network volume** in a data center that has A100 80GB stock,
   e.g. via the RunPod MCP `create-network-volume` (name
   `charcoal-cope-b-weights`, ~70 GB) or the console. Note its data center —
   the endpoint must run in the same one.

3. **Populate the volume** with the weights (once). Attach the volume to a
   cheap temporary pod (CPU or small GPU) and download into it:

   ```bash
   # inside a pod with the volume mounted at /runpod-volume:
   pip install -U "huggingface_hub[cli]"
   huggingface-cli download zentropi-ai/cope-b-a4b \
       --local-dir /runpod-volume/cope-b-a4b
   ```

   Then terminate the pod. The weights persist on the volume.

4. **Create the serverless endpoint** per `runpod.yml`: the GHCR image, the
   network volume mounted at `/runpod-volume`, `MODEL_PATH=/runpod-volume/cope-b-a4b`,
   A100 80GB, FlashBoot, scale-to-zero. Register GHCR pull credentials first
   (`create-container-registry-auth`) since the package is private.

Updating `runpod.yml` requires reconciling the live endpoint (no IaC yet).

## Policy changes

Editing `policy.txt` requires an image rebuild (weights are untouched — only
the tiny app layer rebuilds). CI bumps `POLICY_VERSION` to
`policy-<short-sha>-<date>` automatically. The audit log captures
`policy_version` per classification so a change can be located post-hoc.

## Region

Endpoint and its network volume must be in the **same** data center. Prefer a
`us-west` DC to minimize round-trip from Railway production (verify Railway's
region with `railway status`), but the binding constraint is A100 80GB
availability + the volume's DC.
