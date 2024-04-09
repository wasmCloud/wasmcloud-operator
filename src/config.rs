use serde::{Deserialize, Serialize};

/// Configuration for the operator. If you are configuring the operator using environment variables
/// then all values need to be prefixed with "WASMCLOUD_OPERATOR".
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct OperatorConfig {
    #[serde(default = "default_stream_replicas")]
    pub stream_replicas: u16,
}

fn default_stream_replicas() -> u16 {
    1
}
