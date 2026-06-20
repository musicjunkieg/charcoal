# Pinned vLLM image; matches the version pinned in requirements.txt. The
# container digest is captured in CI when the image is published — see
# .github/workflows/build-cope-b-image.yml for the digest pin used at deploy.
# v0.23.0 patches the v0.20.2 CVEs GHSA-q8gq-377p-jq3r (assert-bypass RCE) and
# GHSA-5jv2-g5wq-cmr4 (GGUF int-truncation), both fixed in 0.22.0+. The
# AsyncLLMEngine API used by handler.py must be revalidated at the GPU smoke
# test, since it can shift across vLLM minor versions.
FROM vllm/vllm-openai:v0.23.0

WORKDIR /app

# Weights are NOT baked into the image. The CoPE-B-A4B model is ~50 GB
# (Gemma-4 26B-A4B, 25B params) — too large to build into an image on
# standard CI runners, and re-pulling a 60 GB image on every cold start is
# wasteful. Instead the weights live on a RunPod **network volume** mounted at
# /runpod-volume at runtime; MODEL_PATH points there. The volume is populated
# once (see gpu/cope-b-runpod/README) and persists across cold starts, so the
# spec's "download the weights only once" goal is preserved without baking.
#
# Layer ordering still matters: stable deps FIRST, frequently-edited app code
# (esp. policy.txt) LAST, so a policy edit rebuilds only the tiny final COPY.

# 1. Stable deps. vllm is already in the base image; we add runpod and pin
#    transformers explicitly. CoPE-B-A4B (gemma4_text / Gemma4ForCausalLM) was
#    saved with transformers 5.8.1 and its tokenizer uses the 5.x-only format
#    (extra_special_tokens as a list + model_specific_special_tokens) — a 4.x
#    transformers crashes loading it ("'list' object has no attribute 'keys'").
#    Use >=5.8 (no upper cap) so we keep vLLM 0.23.0's already-bundled 5.x and
#    never downgrade it.
COPY requirements.txt /app/requirements.txt
RUN python3 -m pip install --no-cache-dir \
        'runpod>=1.7,<2' \
        'transformers>=5.8' \
        huggingface-hub

# 2. Runtime config. MODEL_PATH defaults to the network-volume mount; the
#    endpoint can override it. vLLM loads the model directly from this local
#    directory (no download at boot because the volume is pre-populated).
ENV MODEL_PATH=/runpod-volume/cope-b-a4b \
    POLICY_PATH=/app/policy.txt

# 3. App code LAST — most frequently changed input (especially policy.txt),
#    so keeping it here means a policy edit rebuilds only this tiny layer.
COPY handler.py prompt.py policy.txt /app/

# POLICY_VERSION is injected at build time by CI from the git short SHA + date.
ARG POLICY_VERSION=policy-unversioned
ENV POLICY_VERSION=${POLICY_VERSION}

# The vllm/vllm-openai base image ships its own ENTRYPOINT (the vLLM OpenAI API
# server). Without resetting it, our CMD is appended as *args* to that server and
# handler.py never runs — so runpod.serverless.start never registers a job-taker
# and every request sits IN_QUEUE forever while workers idle/cycle. Reset the
# entrypoint and run our handler as the container's main process.
ENTRYPOINT []
CMD ["python3", "-u", "handler.py"]
