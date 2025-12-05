use k8s_openapi::api::core::v1::{Container, PodSpec, ResourceRequirements, Volume, VolumeMount};
use kube::CustomResource;
use schemars::{gen::SchemaGenerator, schema::Schema, JsonSchema};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "WasmCloudHostConfig",
    group = "k8s.wasmcloud.dev",
    version = "v1alpha1",
    shortname = "whc",
    namespaced,
    status = "WasmCloudHostConfigStatus",
    printcolumn = r#"{"name":"App Count", "type":"integer", "jsonPath":".status.app_count"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct WasmCloudHostConfigSpec {
    /// The number of replicas to use for the wasmCloud host Deployment.
    #[serde(default = "default_host_replicas")]
    pub host_replicas: u32,
    /// DEPRECATED: A list of cluster issuers to use when provisioning hosts. See
    /// https://wasmcloud.com/docs/deployment/security/zero-trust-invocations for more information.
    #[deprecated(since = "0.3.1", note = "Removed in wasmcloud 1.0.0")]
    pub issuers: Option<Vec<String>>,
    /// The lattice to use for these hosts.
    pub lattice: String,
    /// An optional set of labels to apply to these hosts.
    pub host_labels: Option<BTreeMap<String, String>>,
    /// The version of the wasmCloud host to deploy.
    pub version: String,
    /// The image to use for the wasmCloud host.
    /// If not provided, the default image for the version will be used.
    /// Also if provided, the version field will be ignored.
    pub image: Option<String>,
    /// DEPRECATED: Use `natsLeaf.image` instead.
    /// The image to use for the NATS leaf that is deployed alongside the wasmCloud host.
    #[deprecated(note = "Use natsLeaf.image instead")]
    pub nats_leaf_image: Option<String>,
    /// DEPRECATED: Use `natsLeaf.credentialsSecret` instead.
    /// Optional. The name of a secret containing a set of NATS credentials under 'nats.creds' key.
    #[deprecated(note = "Use natsLeaf.credentialsSecret instead")]
    pub secret_name: Option<String>,
    /// Enable structured logging for host logs.
    pub enable_structured_logging: Option<bool>,
    /// Name of a secret containing the registry credentials
    pub registry_credentials_secret: Option<String>,
    /// The control topic prefix to use for the host.
    pub control_topic_prefix: Option<String>,
    /// DEPRECATED: Use `natsLeaf.domain` instead.
    /// The leaf node domain to use for the NATS sidecar. Defaults to "leaf".
    #[deprecated(note = "Use natsLeaf.domain instead")]
    #[serde(default = "default_leaf_node_domain")]
    pub leaf_node_domain: String,
    /// Enable the config service for this host.
    #[serde(default)]
    pub config_service_enabled: bool,
    /// DEPRECATED: Use `natsLeaf.address` instead.
    /// The address of the NATS server to connect to. Defaults to "nats://nats.default.svc.cluster.local".
    #[deprecated(note = "Use natsLeaf.address instead")]
    #[serde(default = "default_nats_address")]
    pub nats_address: String,
    /// DEPRECATED: Use `natsLeaf.clientPort` instead.
    /// The port of the NATS server to connect to. Defaults to 4222.
    #[deprecated(note = "Use natsLeaf.clientPort instead")]
    #[serde(default = "default_nats_port")]
    pub nats_client_port: u16,
    /// DEPRECATED: Use `natsLeaf.leafnodePort` instead.
    /// The port of the NATS server to connect to for leaf node connections. Defaults to 7422.
    #[deprecated(note = "Use natsLeaf.leafnodePort instead")]
    #[serde(default = "default_nats_leafnode_port")]
    pub nats_leafnode_port: u16,
    /// DEPRECATED: Use `natsLeaf.jetstreamDomain` instead.
    /// The Jetstream domain to use for the NATS sidecar. Defaults to "default".
    #[deprecated(note = "Use natsLeaf.jetstreamDomain instead")]
    #[serde(default = "default_jetstream_domain")]
    pub jetstream_domain: String,
    /// Allow the host to deploy using the latest tag on OCI components or providers
    #[serde(default)]
    pub allow_latest: bool,
    /// Allow the host to pull artifacts from OCI registries insecurely
    #[serde(default)]
    pub allowed_insecure: Option<Vec<String>>,
    /// The log level to use for the host. Defaults to "INFO".
    #[serde(default = "default_log_level")]
    pub log_level: String,
    pub policy_service: Option<PolicyService>,
    /// Kubernetes scheduling options for the wasmCloud host.
    pub scheduling_options: Option<KubernetesSchedulingOptions>,
    /// Observability options for configuring the OpenTelemetry integration
    pub observability: Option<ObservabilityConfiguration>,
    /// Certificates: Authorities, client certificates
    pub certificates: Option<WasmCloudHostCertificates>,
    /// wasmCloud secrets topic prefix, must not be empty if set.
    pub secrets_topic_prefix: Option<String>,
    /// Maximum memory in bytes that components can use.
    pub max_linear_memory_bytes: Option<u32>,
    /// Configuration for the NATS leaf node sidecar.
    pub nats_leaf: Option<NatsLeafConfig>,
}

#[allow(deprecated)]
impl WasmCloudHostConfigSpec {
    pub fn effective_nats_leaf_image(&self) -> Option<String> {
        self.nats_leaf
            .as_ref()
            .and_then(|n| n.image.clone())
            .or_else(|| self.nats_leaf_image.clone())
    }

    pub fn effective_credentials_secret(&self) -> Option<String> {
        self.nats_leaf
            .as_ref()
            .and_then(|n| n.credentials_secret.clone())
            .or_else(|| self.secret_name.clone())
    }

    pub fn effective_leaf_node_domain(&self) -> String {
        self.nats_leaf
            .as_ref()
            .and_then(|n| n.domain.clone())
            .unwrap_or_else(|| self.leaf_node_domain.clone())
    }

    pub fn effective_nats_address(&self) -> String {
        self.nats_leaf
            .as_ref()
            .and_then(|n| n.address.clone())
            .unwrap_or_else(|| self.nats_address.clone())
    }

    pub fn effective_nats_client_port(&self) -> u16 {
        self.nats_leaf
            .as_ref()
            .and_then(|n| n.client_port)
            .unwrap_or(self.nats_client_port)
    }

    pub fn effective_nats_leafnode_port(&self) -> u16 {
        self.nats_leaf
            .as_ref()
            .and_then(|n| n.leafnode_port)
            .unwrap_or(self.nats_leafnode_port)
    }

    pub fn effective_jetstream_domain(&self) -> String {
        self.nats_leaf
            .as_ref()
            .and_then(|n| n.jetstream_domain.clone())
            .unwrap_or_else(|| self.jetstream_domain.clone())
    }

    pub fn nats_leaf_tls(&self) -> Option<&NatsLeafTlsConfig> {
        self.nats_leaf.as_ref().and_then(|n| n.tls.as_ref())
    }

    pub fn nats_leaf_extra_volumes(&self) -> Vec<Volume> {
        self.nats_leaf
            .as_ref()
            .and_then(|n| n.extra_volumes.clone())
            .unwrap_or_default()
    }

    pub fn nats_leaf_extra_volume_mounts(&self) -> Vec<VolumeMount> {
        self.nats_leaf
            .as_ref()
            .and_then(|n| n.extra_volume_mounts.clone())
            .unwrap_or_default()
    }
}

/// Configuration for the NATS leaf node sidecar container.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NatsLeafConfig {
    /// The image to use for the NATS leaf container.
    /// If not provided, the default upstream image will be used.
    pub image: Option<String>,
    /// The leaf node domain to use for the NATS sidecar.
    pub domain: Option<String>,
    /// The address of the NATS server to connect to.
    pub address: Option<String>,
    /// The port of the NATS server to connect to.
    pub client_port: Option<u16>,
    /// The port of the NATS server to connect to for leaf node connections.
    pub leafnode_port: Option<u16>,
    /// The Jetstream domain to use for the NATS sidecar.
    pub jetstream_domain: Option<String>,
    /// The name of a secret containing NATS credentials under 'nats.creds' key.
    pub credentials_secret: Option<String>,
    /// TLS configuration for the NATS leaf node connection.
    pub tls: Option<NatsLeafTlsConfig>,
    /// Extra volumes to add to the pod for the NATS leaf container.
    pub extra_volumes: Option<Vec<Volume>>,
    /// Extra volume mounts to add to the NATS leaf container.
    pub extra_volume_mounts: Option<Vec<VolumeMount>>,
}

/// TLS configuration for the NATS leaf node connection.
/// Paths should point to files mounted via `extraVolumes` and `extraVolumeMounts`.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct NatsLeafTlsConfig {
    /// Path to the CA certificate file for verifying the server's certificate.
    pub ca: Option<String>,
    /// Path to the client certificate file for mTLS authentication.
    pub cert: Option<String>,
    /// Path to the client private key file for mTLS authentication.
    pub key: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PolicyService {
    pub topic: Option<String>,
    pub timeout_ms: Option<u32>,
    pub changes_topic: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KubernetesSchedulingOptions {
    /// Run hosts as a DaemonSet instead of a Deployment.
    #[serde(default)]
    pub daemonset: bool,
    /// Kubernetes resources to allocate for the host. See
    /// https://kubernetes.io/docs/concepts/configuration/manage-resources-containers/ for valid
    /// values to use here.
    pub resources: Option<WasmCloudHostConfigResources>,
    /// Any other pod template spec options to set for the underlying wasmCloud host pods.
    #[schemars(schema_with = "pod_schema")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_template_additions: Option<PodSpec>,
    /// Allow for customization of either the wasmcloud or nats leaf container inside of the wasmCloud host pod.
    pub container_template_additions: Option<ContainerTemplateAdditions>,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContainerTemplateAdditions {
    #[schemars(schema_with = "container_schema")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nats: Option<Container>,
    #[schemars(schema_with = "container_schema")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wasmcloud: Option<Container>,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ObservabilityConfiguration {
    #[serde(default)]
    pub enable: bool,
    pub endpoint: String,
    pub protocol: Option<OtelProtocol>,
    pub logs: Option<OtelSignalConfiguration>,
    pub metrics: Option<OtelSignalConfiguration>,
    pub traces: Option<OtelSignalConfiguration>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum OtelProtocol {
    Grpc,
    Http,
}

impl std::fmt::Display for OtelProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                OtelProtocol::Grpc => "grpc",
                OtelProtocol::Http => "http",
            }
        )
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OtelSignalConfiguration {
    pub enable: Option<bool>,
    pub endpoint: Option<String>,
}

/// This is a workaround for the fact that we can't override the PodSpec schema to make containers
/// an optional field. It generates the OpenAPI schema for the PodSpec type the same way that
/// kube.rs does while dropping any required fields.
fn pod_schema(_gen: &mut SchemaGenerator) -> Schema {
    let gen = schemars::gen::SchemaSettings::openapi3()
        .with(|s| {
            s.inline_subschemas = true;
            s.meta_schema = None;
        })
        .with_visitor(kube::core::schema::StructuralSchemaRewriter)
        .into_generator();
    let mut val = gen.into_root_schema_for::<PodSpec>();
    // Drop `containers` as a required field, along with any others.
    val.schema.object.as_mut().unwrap().required = BTreeSet::new();
    val.schema.into()
}

/// This is a workaround for the fact that we can't override the Container schema to make name
/// an optional field. It generates the OpenAPI schema for the Container type the same way that
/// kube.rs does while dropping any required fields.
fn container_schema(_gen: &mut SchemaGenerator) -> Schema {
    let gen = schemars::gen::SchemaSettings::openapi3()
        .with(|s| {
            s.inline_subschemas = true;
            s.meta_schema = None;
        })
        .with_visitor(kube::core::schema::StructuralSchemaRewriter)
        .into_generator();
    let mut val = gen.into_root_schema_for::<Container>();
    // Drop `name` as a required field as it will be filled in from container
    // definition coming the controller that this configuration gets merged into.
    val.schema.object.as_mut().unwrap().required = BTreeSet::new();
    val.schema.into()
}

fn default_host_replicas() -> u32 {
    1
}

fn default_jetstream_domain() -> String {
    "default".to_string()
}

fn default_nats_address() -> String {
    "nats://nats.default.svc.cluster.local".to_string()
}

fn default_leaf_node_domain() -> String {
    "leaf".to_string()
}

fn default_log_level() -> String {
    "INFO".to_string()
}

fn default_nats_port() -> u16 {
    4222
}

fn default_nats_leafnode_port() -> u16 {
    7422
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct WasmCloudHostCertificates {
    pub authorities: Option<Vec<Volume>>,
}

impl NatsLeafTlsConfig {
    pub fn is_enabled(&self) -> bool {
        self.ca.is_some() || self.cert.is_some() || self.key.is_some()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct WasmCloudHostConfigResources {
    pub nats: Option<ResourceRequirements>,
    pub wasmcloud: Option<ResourceRequirements>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct WasmCloudHostConfigStatus {
    pub apps: Vec<AppStatus>,
    pub app_count: u32,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct AppStatus {
    pub name: String,
    pub version: String,
}
