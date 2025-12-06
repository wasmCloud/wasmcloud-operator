# wadm-operator

> **Note**: This operator is for wasmCloud v1 only and is not compatible with wasmCloud v2.

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
  # The lattice to connect the hosts to
  lattice: default
  # Additional labels to apply to the host other than the defaults set in the operator
  hostLabels:
    some-label: value
  # The address to connect to nats
  natsAddress: nats://nats.default.svc.cluster.local
  # Which wasmCloud version to use
  version: 1.0.4
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
#data:
# Only required if using a NATS creds file
# nats.creds: <creds file>
```

The operator will fail to provision the wasmCloud Deployment if any of these
secrets are missing!

#### Customizing the images used for wasmCloud host and NATS leaf

If you would like to customize the registry or image that gets used to provision the wasmCloud hosts and the NATS leaf that runs alongside them, you can specify the following options in the above `WasmCloudHostConfig` CRD.

For wasmCloud Host, use the `image` field:

```yaml
apiVersion: k8s.wasmcloud.dev/v1alpha1
kind: WasmCloudHostConfig
metadata:
  name: my-wasmcloud-cluster
spec:
  # other config options omitted
  image: registry.example.com/wasmcloud:1.0.2
```

For the NATS leaf sidecar, use the `natsLeaf` configuration block:

```yaml
apiVersion: k8s.wasmcloud.dev/v1alpha1
kind: WasmCloudHostConfig
metadata:
  name: my-wasmcloud-cluster
spec:
  # other config options omitted
  natsLeaf:
    image: registry.example.com/nats:2.10.16
    address: nats://nats.default.svc.cluster.local
    credentialsSecret: wasmcloud-nats-creds
    jetstreamDomain: default
```

#### Configuring TLS for the NATS leaf connection

If your NATS server uses TLS, you can configure the leaf node to verify the server certificate by providing a CA certificate. Use `extraVolumes` and `extraVolumeMounts` to mount the certificate into the container:

```yaml
apiVersion: k8s.wasmcloud.dev/v1alpha1
kind: WasmCloudHostConfig
metadata:
  name: my-wasmcloud-cluster
spec:
  # other config options omitted
  natsLeaf:
    address: nats://nats.example.com
    credentialsSecret: wasmcloud-nats-creds
    jetstreamDomain: default
    tls:
      ca: /nats/ca/ca-certificates.crt
    extraVolumes:
      - name: nats-ca
        configMap:
          name: my-ca-bundle
    extraVolumeMounts:
      - name: nats-ca
        mountPath: /nats/ca
        readOnly: true
```

For mTLS (mutual TLS), you can also provide client certificate and key paths:

```yaml
    tls:
      ca: /nats/ca/ca-certificates.crt
      cert: /nats/certs/tls.crt
      key: /nats/certs/tls.key
```

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
helm repo add nats https://nats-io.github.io/k8s/helm/charts/
helm upgrade --install -f values.yaml nats nats/nats
```

### Running Wadm

You can run Wadm in your Kubernetes cluster using our Helm chart. For a minimal deployment using the
NATS server deployed above, all you need in your `values.yaml` file is:

```yaml
wadm:
  config:
    nats:
      server: "nats.default.svc.cluster.local:4222"
```

You can deploy Wadm using your values file and Helm:

```sh
helm install wadm -f wadm-values.yaml --version 0.2.0 oci://ghcr.io/wasmcloud/charts/wadm
```

### Start the operator

```sh
kubectl kustomize deploy/base | kubectl apply -f -
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
crate (`wadm-operator-types`) so that they can be reused in other projects.
