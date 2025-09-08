#!/bin/sh
set -o errexit

# 1. Delete kind cluster
kind delete cluster -n wasmcloud-operator-test

# 2. Delete local registry
docker stop kind-registry
docker rm kind-registry
