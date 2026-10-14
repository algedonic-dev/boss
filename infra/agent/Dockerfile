# Source recipe only. No build, push, digest or login is implied by this car.
# Uses the already-approved forge-mirrored toolchain; publication must bind
# the resulting OCI platform digest and this image's exact witness together.
# Required publisher input: the approved forge-mirrored boss-ci artifact,
# resolved and digest-verified through the existing image publisher door.
ARG TOOLCHAIN_IMAGE
FROM ${TOOLCHAIN_IMAGE}
RUN apt-get update && apt-get install -y --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/*
RUN npm install --global --ignore-scripts @openai/codex@0.159.3 \
    && mkdir -p /usr/share/boss-agent \
    && npm list --global --all --json > /usr/share/boss-agent/package-closure.json \
    && codex --version > /usr/share/boss-agent/version.txt \
    && codex exec --help > /usr/share/boss-agent/exec-help.txt \
    && grep -F '0.159.3' /usr/share/boss-agent/version.txt \
    && grep -F -- '--ignore-user-config' /usr/share/boss-agent/exec-help.txt \
    && grep -F -- '--output-last-message' /usr/share/boss-agent/exec-help.txt \
    && grep -F -- '--sandbox' /usr/share/boss-agent/exec-help.txt
COPY infra/agent/agent-io.py /usr/local/bin/boss-agent-io
RUN chmod 0555 /usr/local/bin/boss-agent-io \
    && sha256sum /usr/local/bin/boss-agent-io /usr/share/boss-agent/* > /usr/share/boss-agent/witness.sha256
USER 65534:1500
WORKDIR /workspace
CMD ["/bin/sleep", "infinity"]
