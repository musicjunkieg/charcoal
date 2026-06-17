# Pinned vLLM image; matches the version pinned in requirements.txt. The
# container digest is captured in CI when the image is published — see
# .github/workflows/build-cope-b-image.yml for the digest pin used at deploy.
FROM vllm/vllm-openai:v0.20.2

WORKDIR /app

# Layer ordering matters: Docker cache keys are cumulative, so the stable,
# expensive layers must come FIRST and the frequently-edited app code LAST.
# Editing policy.txt then only rebuilds the tiny final COPY — the ~50 GB
# weight-download layer stays cached because nothing before it changed.

# 1. Stable deps. vllm is already in the base image; we add runpod and
#    transformers explicitly so handler+prompt imports don't depend on
#    whatever transformers version the base image happens to ship (or lack).
COPY requirements.txt /app/requirements.txt
RUN python3 -m pip install --no-cache-dir \
        'runpod>=1.7,<2' \
        'transformers>=4.50,<5' \
        huggingface-hub

# 2. Bake weights into the image. This layer is keyed only by MODEL_REVISION
#    (the only input above it that varies), so a policy.txt-only edit never
#    invalidates it. Build-arg lets CI override the revision for pinned builds.
ARG MODEL_REVISION=main
ENV MODEL_PATH=/weights \
    POLICY_PATH=/app/policy.txt
RUN huggingface-cli download zentropi-ai/cope-b-a4b \
        --local-dir /weights \
        --revision ${MODEL_REVISION}

# 3. App code LAST — this is the most frequently changed input (especially
#    policy.txt), so keeping it here means a policy edit rebuilds only this
#    tiny layer, not the weight download above.
COPY handler.py prompt.py policy.txt /app/

# POLICY_VERSION is injected at build time by CI from the git short SHA + date.
ARG POLICY_VERSION=policy-unversioned
ENV POLICY_VERSION=${POLICY_VERSION}

CMD ["python3", "-u", "handler.py"]
