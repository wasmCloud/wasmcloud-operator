repo := ghcr.io/wasmcloud/wadm-operator
version := $(shell git rev-parse --short HEAD)
platforms := linux/amd64,linux/arm64

.PHONY: build-dev-image build-image buildx-image
build-image:
	docker build -t $(repo):$(version) .

buildx-image:
	docker buildx build --platform $(platforms) -t $(repo):$(version) --load .

build-dev-image:
	docker build -t $(repo):$(version)-dev -f Dockerfile.local .
