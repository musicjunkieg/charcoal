# Pinned vLLM image; matches the version pinned in requirements.txt. The
# container digest is captured in CI when the image is published — see
# .github/workflows/build-cope-b-image.yml for the digest pin used at deploy.
FROM vllm/vllm-openai:v0.20.2

WORKDIR /app

# Copy app code first so requirements layer caches on weight changes (which
# are the slow part). vllm is already in the base image; we add runpod and
# transformers explicitly so handler+prompt imports don't depend on whatever
# transformers version the base image happens to ship (or lack).
COPY requirements.txt /app/requirements.txt
RUN python3 -m pip install --no-cache-dir \
        'runpod>=1.7,<2' \
        'transformers>=4.50,<5'

COPY handler.py prompt.py policy.txt /app/

# Bake weights into the image. Build-arg lets CI override the revision for
# pinned-version builds.
ARG MODEL_REVISION=main
ENV MODEL_PATH=/weights \
    POLICY_PATH=/app/policy.txt
RUN python3 -m pip install --no-cache-dir huggingface-hub && \
    huggingface-cli download zentropi-ai/cope-b-a4b \
        --local-dir /weights \
        --revision ${MODEL_REVISION}

# POLICY_VERSION is injected at build time by CI from the git short SHA + date.
ARG POLICY_VERSION=policy-unversioned
ENV POLICY_VERSION=${POLICY_VERSION}

CMD ["python3", "-u", "handler.py"]
