# syntax=docker/dockerfile:1
FROM gcr.io/distroless/cc-debian12
ARG BIN_PATH
ARG TARGETARCH

COPY ${BIN_PATH}-${TARGETARCH} /usr/local/bin/wasmcloud-operator
ENTRYPOINT ["/usr/local/bin/wasmcloud-operator"]
