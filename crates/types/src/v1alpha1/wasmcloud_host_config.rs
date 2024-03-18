use k8s_openapi::api::core::v1::ResourceRequirements;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "WasmCloudHostConfig",
    group = "k8s.wasmcloud.dev",
    version = "v1alpha1",
    shortname = "chc",
    namespaced,
    status = "WasmCloudHostConfigStatus",
    printcolumn = r#"{"name":"App Count", "type":"integer", "jsonPath":".status.app_count"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct WasmCloudHostConfigSpec {
    pub host_replicas: u32,
    pub issuers: Vec<String>,
    pub lattice: String,
    pub host_labels: Option<HashMap<String, String>>,
    pub version: String,
    pub secret_name: String,
    pub enable_structured_logging: Option<bool>,
    pub registry_credentials_secret: Option<String>,
    pub resources: Option<WasmCloudHostConfigResources>,
    pub control_topic_prefix: Option<String>,
    #[serde(default = "default_leaf_node_domain")]
    pub leaf_node_domain: String,
    #[serde(default)]
    pub config_service_enabled: bool,
    #[serde(default = "default_nats_address")]
    pub nats_address: String,
    #[serde(default = "default_jetstream_domain")]
    pub jetstream_domain: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
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
