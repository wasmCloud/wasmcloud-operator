# wasmcloud-operator

An operator for managing a set of [wasmCloud hosts](https://github.com/wasmCloud/wasmCloud/) running on Kubernetes and
manage [wasmCloud applications using wadm](https://github.com/wasmcloud/wadm).
The goal is to easily be able to run WasmCloud hosts on a Kubernetes cluster.

## WasmCloudHostConfig Custom Resource Definition (CRD)

The WasmCloudHostConfig CRD describes the desired state of a set of wasmCloud
hosts connected to the same lattice.

```yaml
apiVersion: k8s.wasmcloud.dev/v1alpha1
kind: WasmCloudHostConfig
metadata:
  name: my-wasmcloud-cluster
spec:
  # The number of wasmCloud host pods to run
  hostReplicas: 2
  # The cluster issuers to use for each host
  issuers:
    # This needs to be the public key of a cluster seed generated by
    # `wash keys gen cluster`
    - CDK...
  # The lattice to connect the hosts to
  lattice: 83a5b52e-17cf-4080-bac8-f844099f142e
  # Additional labels to apply to the host other than the defaults set in the operator
  hostLabels:
    some-label: value
  # Which wasmCloud version to use
  version: 0.82.0
  # The name of a secret in the same namespace that provides the required secrets.
  secretName: cluster-secrets
  # Enable the following to run the wasmCloud hosts as a DaemonSet
  #daemonset: true
  # The name of the image pull secret to use with wasmCloud hosts so that they
  # can authenticate to a private registry to pull components.
  # registryCredentialsSecret: my-registry-secret
```

The CRD requires a Kubernetes Secret with the following keys:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: my-wasmcloud-cluster
data:
  # You can generate this with wash:
  # `wash keys gen cluster`
  WASMCLOUD_CLUSTER_SEED: <seed>
  # Only required if using a NATS creds file
  # nats.creds: <base64 encoded creds file>
```

The operator will fail to provision the wasmCloud Deployment if any of these
secrets are missing!

### Image Pull Secrets

You can also specify an image pull secret to use use with the wasmCloud hosts
so that they can pull components from a private registry. This secret needs to
be in the same namespace as the WasmCloudHostConfig CRD and must be a
`kubernetes.io/dockerconfigjson` type secret. See the [Kubernetes
documentation](https://kubernetes.io/docs/tasks/configure-pod-container/pull-image-private-registry/#registry-secret-existing-credentials)
for more information on how to provision that secret.

Once it is created, you can reference it in the WasmCloudHostConfig CRD by
setting the `registryCredentialsSecret` field to the name of the secret.

## Deploying the operator

A wasmCloud cluster requires a few things to run:

- A NATS cluster with Jetstream enabled
- WADM connected to the NATS cluster in order to support applications

If you are running locally, you can use the following commands to start a
NATS cluster and WADM in your Kubernetes cluster.

### Running NATS

Use the upstream NATS Helm chart to start a cluster with the following
values.yaml file:

```yaml
config:
  cluster:
    enabled: true
    replicas: 3
  leafnodes:
    enabled: true
  jetstream:
    enabled: true
    fileStore:
      pvc:
        size: 10Gi
    merge:
      domain: default
```

```sh
helm upgrade --install -f values.yaml nats-cluster nats/nats
```

### Running Wadm

You can deploy Wadm to your Kubernetes cluster with the Wadm Helm chart. First, clone the Wadm repo from GitHub:

```shell
git clone https://github.com/wasmCloud/wadm.git
```

In the cloned repo, find the chart at `wadm/charts/wadm`. For a minimal deployment, all you need in your `values.yaml` file is:

```yaml
wadm:
  config:
    nats:
      server: "nats-cluster.default.svc.cluster.local:4222"
```
```sh
helm install wadm -f values.yaml ./
```

### Start the operator

```sh
kubectl kustomize build deploy/local | kubectl apply -f -
```

## Automatically Syncing Kubernetes Services

The operator automatically creates Kubernetes Services for wasmCloud
applications. Right now this is limited only to applications that deploy the
wasmCloud httpserver component using a `daemonscaler`, but additional support
for `spreadscalers` will be added in the future.

If you specify host label selectors on the `daemonscaler` then the operator
will honor those labels and will only create a service for the pods that match
those label selectors.

## Argo CD Health Check

Argo CD provides a way to define a [custom health
check](https://argo-cd.readthedocs.io/en/stable/operator-manual/health/#custom-health-checks)
that it then runs against a given resource to determine whether or not the
resource is in healthy state.

For this purpose, we specifically expose a `status.phase` field, which exposes
the underlying status information from wadm.

With the following ConfigMap, a custom health check can be added to an existing
Argo CD installation for tracking the health of wadm applications.

```yaml
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: argocd-cm
  namespace: argocd
  labels:
    app.kubernetes.io/name: argocd-cm
    app.kubernetes.io/part-of: argocd
data:
  resource.customizations: |
    core.oam.dev/Application:
      health.lua: |
        hs = {}
        hs.status = "Progressing"
        hs.message = "Reconciling application state"
        if obj.status ~= nil and obj.status.phase ~= nil then
          if obj.status.phase == "Deployed" then
            hs.status = "Healthy"
            hs.message = "Application is ready"
          end
          if obj.status.phase == "Reconciling" then
            hs.status = "Progressing"
            hs.message = "Application has been deployed"
          end
          if obj.status.phase == "Failed" then
            hs.status = "Degraded"
            hs.message = "Application failed to deploy"
          end
          if obj.status.phase == "Undeployed" then
            hs.status = "Suspended"
            hs.message = "Application is undeployed"
          end
        end
        return hs
```

## Testing

- Make sure you have a Kubernetes cluster running locally. Some good options
  include [Kind](https://kind.sigs.k8s.io/) or Docker Desktop.
- `RUST_LOG=info cargo run`


## Types crate

This repo stores the types for any CRDs used by the operator in a separate
crate (`wasmcloud-operator-types`) so that they can be reused in other projects.
