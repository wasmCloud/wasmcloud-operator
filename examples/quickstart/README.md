# Example setup

This example shows the bare minimum requirements to deploy applications on wasmCloud.

It relies on the Kubernetes `default` namespace for simplicity.

## Install [NATS](https://github.com/nats-io/nats-server)

```bash
helm repo add nats https://nats-io.github.io/k8s/helm/charts/
helm upgrade --install -f nats-values.yaml nats nats/nats
```

Validate installation with:

```bash
# make sure pods are ready
kubectl rollout status deploy,sts -l app.kubernetes.io/instance=nats
```

## Install wasmCloud Application Deployment Manager - [wadm](https://github.com/wasmCloud/wadm)

```sh
helm install wadm -f wadm-values.yaml oci://ghcr.io/wasmcloud/charts/wadm
```

Validate installation with:

```bash
# make sure pods are ready
kubectl rollout status deploy -l app.kubernetes.io/instance=wadm
```

## Install the operator

```sh
kubectl apply -k ../../deploy/base
```

Validate installation with:

```bash
# make sure pods are ready
kubectl rollout status deploy -l app=wadm-operator -n wadm-operator
# apiservice should be available
kubectl get apiservices.apiregistration.k8s.io v1beta1.core.oam.dev
```

## Create wasmcloud cluster

```bash
kubectl apply -f wasmcloud-host.yaml
```

Check wasmCloud host status with:

```bash
kubectl describe wasmcloudhostconfig wasmcloud-host
```

## Managing applications using kubectl

Install the rust hello world application:

```bash
kubectl apply -f hello-world-application.yaml
```

Check application status with:

```bash
kubectl get applications
```

## Managing applications with wash

Port forward into the NATS cluster. 4222 = NATS Service, 4223 = NATS Websockets

```bash
kubectl port-forward svc/nats 4222:4222 4223:4223
```

In another shell:

```bash
wash app list
```
