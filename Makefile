repo := ghcr.io/wasmcloud/wasmcloud-operator
local_repo := localhost:5001/wasmcloud-operator
version := $(shell git rev-parse --short HEAD)
platforms := linux/amd64,linux/arm64

.PHONY: build-dev-image build-image buildx-image setup teardown build-local-dev-image push-local-dev-image deploy-local-dev-image update-local-dev-image
build-image:
	docker build -t $(repo):$(version) .

buildx-image:
	docker buildx build --platform $(platforms) -t $(repo):$(version) --load .

build-dev-image:
	docker build -t $(repo):$(version)-dev -f Dockerfile.local .

setup:
	cd hack && bash ./run-kind-cluster.sh

teardown:
	cd hack && bash ./teardown-kind-cluster.sh

build-local-dev-image:
	docker build -t $(local_repo):latest -f Dockerfile.local .

push-local-dev-image:
	docker push $(local_repo):latest

deploy-local-dev-image:
	kubectl kustomize deploy/local | kubectl apply -f -

# deleting pod makes it pull a new version thanks to pullPolicy always
update-local-dev-image:
	kubectl delete pod -n wasmcloud-operator -l "app=wasmcloud-operator"
