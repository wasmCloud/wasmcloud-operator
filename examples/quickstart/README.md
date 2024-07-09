# Example setup

This example shows the bare minimum requirements to deploy applications on wasmCloud.

It relies on Kubernetes `default` namespace for simplicity.

## Install [NATS](https://github.com/nats-io/nats-server)

```bash
helm repo add nats https://nats-io.github.io/k8s/helm/charts/
helm upgrade --install -f nats-values.yaml nats-cluster nats/nats
```

Validate installation with:

```bash
# make sure pods are ready
kubectl get statefulset,deployment -l app.kubernetes.io/instance=nats-cluster
```

## Install wasmCloud Application Deployment Manager - [wadm](https://github.com/wasmCloud/wadm)

```sh
helm install wadm -f wadm-values.yaml --version 0.2.0 oci://ghcr.io/wasmcloud/charts/wadm
```

Validate installation with:

```bash
# make sure pods are ready
kubectl get deploy wadm
```

## Install the operator

```sh
kubectl apply -k ../deploy/base
```

Validate installation with:

```bash
# make sure pods are ready
kubectl -n wasmcloud-operator get deploy
# apiservice should be available
kubectl get apiservices.apiregistration.k8s.io v1beta1.core.oam.dev
```

## Create wasmcloud cluster (Default Lattice)

```bash
kubectl apply -f default-lattice.yaml
```

Check wasmCloud host status with:

```bash
kubectl describe wasmcloudhostconfig default
```

## Deploying applications using kubectl

Install the rust hello world application:

```bash
kubectl apply -f hello-world-application.yaml
```

Check application status with:

```bash
kubectl get applications
```
