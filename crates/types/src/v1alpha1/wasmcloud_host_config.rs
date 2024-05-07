use k8s_openapi::api::core::v1::{PodSpec, ResourceRequirements};
use kube::CustomResource;
use schemars::{gen::SchemaGenerator, schema::Schema, JsonSchema};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

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
    /// A list of cluster issuers to use when provisioning hosts. See
    /// https://wasmcloud.com/docs/deployment/security/zero-trust-invocations for more information.
    pub issuers: Vec<String>,
    /// The lattice to use for these hosts.
    pub lattice: String,
    /// An optional set of labels to apply to these hosts.
    pub host_labels: Option<HashMap<String, String>>,
    /// The version of the wasmCloud host to deploy.
    pub version: String,
    /// The image to use for the wasmCloud host.
    /// If not provided, the default image for the version will be used.
    /// Also if provided, the version field will be ignored.
    pub image: Option<String>,
    /// The name of a secret containing the primary cluster issuer key along with an optional set
    /// of NATS credentials.
    pub secret_name: String,
    /// Enable structured logging for host logs.
    pub enable_structured_logging: Option<bool>,
    /// Name of a secret containing the registry credentials
    pub registry_credentials_secret: Option<String>,
    /// The control topic prefix to use for the host.
    pub control_topic_prefix: Option<String>,
    /// The leaf node domain to use for the NATS sidecar. Defaults to "leaf".
    #[serde(default = "default_leaf_node_domain")]
    pub leaf_node_domain: String,
    /// Enable the config service for this host.
    #[serde(default)]
    pub config_service_enabled: bool,
    /// The address of the NATS server to connect to. Defaults to "nats://nats.default.svc.cluster.local".
    #[serde(default = "default_nats_address")]
    pub nats_address: String,
    /// The Jetstream domain to use for the NATS sidecar. Defaults to "default".
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
    #[schemars(schema_with = "pod_schema")]
    /// Any other pod template spec options to set for the underlying wasmCloud host pods.
    pub pod_template_additions: Option<PodSpec>,
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
