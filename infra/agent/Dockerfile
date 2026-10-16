# Source recipe only. No build, push, digest or login is implied by this car.
# Uses the already-approved forge-mirrored toolchain; publication must bind
# the resulting OCI platform digest and this image's exact witness together.
# Required publisher input: the approved forge-mirrored boss-ci artifact,
# resolved and digest-verified through the existing image publisher door.
ARG TOOLCHAIN_IMAGE
FROM ${TOOLCHAIN_IMAGE}
ARG CODEX_VERSION
RUN test -n "${CODEX_VERSION}" \
    && npm install --global --prefix /usr/local --ignore-scripts @openai/codex@${CODEX_VERSION}
RUN --network=none python3 --version \
    && mkdir -p /usr/share/boss-agent \
    && npm list --global --prefix /usr/local --all --json > /usr/share/boss-agent/package-closure.json \
    && /usr/local/bin/codex --version > /usr/share/boss-agent/version.txt \
    && /usr/local/bin/codex exec --help > /usr/share/boss-agent/exec-help.txt \
    && grep -Fx "codex-cli ${CODEX_VERSION}" /usr/share/boss-agent/version.txt
COPY infra/agent/agent-io.py /usr/local/bin/boss-agent-io
COPY infra/agent/private-collector.py /usr/local/bin/boss-private-cache-collector
COPY infra/agent/private-collector-contract.json /usr/share/boss-agent/private-collector-contract.json
RUN --network=none chmod 0555 /usr/local/bin/boss-private-cache-collector /usr/local/bin/boss-agent-io \
    && sha256sum /usr/local/bin/boss-agent-io /usr/local/bin/boss-private-cache-collector /usr/share/boss-agent/* > /usr/share/boss-agent/witness.sha256
USER 65534:1500
WORKDIR /workspace
CMD ["/bin/sleep", "infinity"]
