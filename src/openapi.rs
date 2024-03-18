use std::collections::{BTreeMap, HashMap};

use axum::{
    http::{HeaderMap, StatusCode, Uri},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tracing::debug;
use utoipa::{OpenApi, ToSchema};

/* TODO:
 * - Add full support for Kubernetes' OpenAPI spec: https://github.com/kubernetes/kubernetes/tree/master/api/openapi-spec
 * - Add namespace and labels support:
 *  - Without namespaces we get an "InvalidSpecError": https://github.com/argoproj/argo-cd/blob/a761a495f16d76c0a8e50359eda50f605e329aba/controller/state.go#L694-L696
 */

/// An OAM manifest
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
#[schema(as = dev::oam::core::v1beta1::Application)]
#[serde(rename = "Application")]
pub struct Application {
    /// The OAM version of the manifest
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    /// The kind or type of manifest described by the spec
    pub kind: String,
    /// Metadata describing the manifest
    pub metadata: Metadata,
    /// The specification for this manifest
    pub spec: Specification,
}

/// The metadata describing the manifest
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
#[schema(as = dev::oam::core::v1beta1::Metadata)]
pub struct Metadata {
    /// The name of the manifest. This should be unique
    pub name: String,
    // This is to satisfy ArgoCD's validation.
    pub namespace: String,
    /// Optional data for annotating this manifest
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
    // This is to satisfy ArgoCD's validation.
    pub labels: BTreeMap<String, String>,
}

/// A representation of an OAM specification
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
#[schema(as = dev::oam::core::v1beta1::Specification)]
pub struct Specification {
    /// The list of components for describing an application
    pub components: Vec<Component>,
}

/// A component definition
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
#[schema(as = dev::oam::core::v1beta1::Component)]
pub struct Component {
    /// The name of this component
    pub name: String,
    /// The type of component
    /// The properties for this component
    // NOTE(thomastaylor312): It would probably be better for us to implement a custom deserialze
    // and serialize that combines this and the component type. This is good enough for first draft
    #[serde(flatten)]
    pub properties: Properties,
    /// A list of various traits assigned to this component
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traits: Option<Vec<Trait>>,
}

/// Properties that can be defined for a component
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
#[schema(as = dev::oam::core::v1beta1::Properties)]
#[serde(tag = "type")]
pub enum Properties {
    #[serde(rename = "actor")]
    Actor { properties: ActorProperties },
    #[serde(rename = "capability")]
    Capability { properties: CapabilityProperties },
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
#[schema(as = dev::oam::core::v1beta1::ActorProperties)]
pub struct ActorProperties {
    /// The image reference to use
    pub image: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
#[schema(as = dev::oam::core::v1beta1::CapabilityProperties)]
pub struct CapabilityProperties {
    /// The image reference to use
    pub image: String,
    /// The contract ID of this capability
    pub contract: String,
    /// An optional link name to use for this capability
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_name: Option<String>,
    /// Optional config to pass to the provider. This can be either a raw string encoded config, or
    /// a JSON or YAML object
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<CapabilityConfig>,
}

/// Right now providers can technically use any config format they want, although most use JSON.
/// This enum takes that into account and allows either type of data to be passed
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
#[schema(as = dev::oam::core::v1beta1::CapabilityConfig)]
pub enum CapabilityConfig {
    Json(serde_json::Value),
    Opaque(String),
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
#[schema(as = dev::oam::core::v1beta1::Trait)]
pub struct Trait {
    /// The type of trait specified. This should be a unique string for the type of scaler. As we
    /// plan on supporting custom scalers, these traits are not enumerated
    #[serde(rename = "type")]
    pub trait_type: String,
    /// The properties of this trait
    pub properties: TraitProperty,
}

/// Properties for defining traits
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
#[schema(as = dev::oam::core::v1beta1::TraitProperty)]
#[serde(untagged)]
pub enum TraitProperty {
    Linkdef(LinkdefProperty),
    SpreadScaler(SpreadScalerProperty),
    // TODO(thomastaylor312): This is still broken right now with deserializing. If the incoming
    // type specifies replicas, it matches with spreadscaler first. So we need to implement a custom
    // parser here
    Custom(serde_json::Value),
}

/// Properties for linkdefs
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
#[schema(as = dev::oam::core::v1beta1::LinkdefProperty)]
pub struct LinkdefProperty {
    /// The target this linkdef applies to. This should be the name of an actor component
    pub target: String,
    /// Values to use for this linkdef
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<HashMap<String, String>>,
}

/// Properties for spread scalers
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
#[schema(as = dev::oam::core::v1beta1::SpreadScalerProperty)]
pub struct SpreadScalerProperty {
    /// Number of replicas to scale
    pub replicas: usize,
    /// Requirements for spreading throse replicas
    #[serde(default)]
    pub spread: Vec<Spread>,
}

/// Configuration for various spreading requirements
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
#[schema(as = dev::oam::core::v1beta1::Spread)]
pub struct Spread {
    /// The name of this spread requirement
    pub name: String,
    /// An arbitrary map of labels to match on for scaling requirements
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub requirements: BTreeMap<String, String>,
    /// An optional weight for this spread. Higher weights are given more precedence
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<usize>,
}

#[derive(OpenApi)]
#[openapi(
    components(schemas(
        Application,
        Metadata,
        Specification,
        Component,
        Properties,
        ActorProperties,
        CapabilityProperties,
        CapabilityConfig,
        Trait,
        TraitProperty,
        LinkdefProperty,
        SpreadScalerProperty,
        Spread,
    )),
    info(
        description = "The OAM Application API provides a way to manage applications in a Kubernetes cluster."
    ),
    paths(
        crate::router::api_resources,
        crate::resources::application::create_application,
        crate::resources::application::list_applications,
        crate::resources::application::get_application,
        crate::resources::application::patch_application,
        crate::resources::application::delete_application,
    )
)]
pub struct ApiDoc;

#[derive(Serialize)]
struct OpenApiV3Discovery {
    paths: HashMap<String, OpenApiV3DiscoveryGroupVersion>,
}

#[derive(Serialize)]
struct OpenApiV3DiscoveryGroupVersion {
    #[serde(rename = "serverRelativeURL")]
    server_relative_url: String,
}

impl OpenApiV3Discovery {
    pub fn new() -> Self {
        let mut paths = HashMap::new();
        paths.insert(
            "apis/core.oam.dev/v1beta1".to_string(),
            OpenApiV3DiscoveryGroupVersion {
                server_relative_url: "/openapi/v3/apis/core.oam.dev/v1beta1".to_string(),
            },
        );
        Self { paths }
    }
}

pub fn router() -> Router {
    Router::new()
        .route("/v3", get(openapi_v3))
        .route("/v3/apis/core.oam.dev/v1beta1", get(openapi_v3_details))
        .route("/v2", get(openapi_v2))
        .fallback(fallback)
}

async fn fallback(headers: HeaderMap, uri: Uri) -> (StatusCode, String) {
    debug!("openapi fallback: uri={uri}");
    for (hk, hv) in headers.iter() {
        debug!("hk={hk}, hv={}", hv.to_str().unwrap())
    }
    (StatusCode::NOT_FOUND, format!("No route for {uri}"))
}

async fn openapi_v3() -> Json<OpenApiV3Discovery> {
    //let doc = ApiDoc::openapi().to_json().unwrap();
    let root = OpenApiV3Discovery::new();
    Json(root)
}

async fn openapi_v3_details() -> Json<serde_json::Value> {
    // TODO add actual OAM docs in this
    // We may need to copy/paste the OAM spec from wadm here to add the right annotations or at
    // least use type annotation.
    let doc = ApiDoc::openapi();
    Json(serde_json::to_value(doc).unwrap())
}

async fn openapi_v2() -> impl IntoResponse {
    OPENAPI_V2_SPEC_JSON.into_response()
}

const OPENAPI_V2_SPEC_JSON: &str = r##"
{
    "info": {
      "title": "wasmcloud-operator",
      "description": "The OAM Application API provides a way to manage applications in a Kubernetes cluster.",
      "license": {
        "name": ""
      },
      "version": "0.1.6"
    },
    "paths": {
      "/apis/core.oam.dev/v1beta1": {
        "get": {
          "tags": [
            "crate::router"
          ],
          "operationId": "api_resources",
          "responses": {},
          "parameters": []
        }
      },
      "/apis/core.oam.dev/v1beta1/namespaces/{namespace}/applications": {
        "get": {
          "tags": [
            "crate::resources::application"
          ],
          "operationId": "list_applications",
          "parameters": [
            {
              "name": "namespace",
              "in": "path",
              "required": true,
              "type": "string"
            }
          ],
          "responses": {},
          "x-kubernetes-group-version-kind": [
            {
              "group": "core.oam.dev",
              "kind": "Application",
              "version": "v1beta1"
            }
          ]
        },
        "post": {
          "tags": [
            "crate::resources::application"
          ],
          "operationId": "create_application",
          "parameters": [
            {
              "name": "namespace",
              "in": "path",
              "required": true,
              "type": "string"
            },
            {
              "description": "",
              "required": true,
              "name": "body",
              "in": "body",
              "schema": {
                "type": "string",
                "format": "binary"
              }
            }
          ],
          "responses": {},
          "consumes": [
            "application/octet-stream"
          ],
          "x-kubernetes-group-version-kind": [
            {
              "group": "core.oam.dev",
              "kind": "Application",
              "version": "v1beta1"
            }
          ]
        }
      },
      "/apis/core.oam.dev/v1beta1/namespaces/{namespace}/applications/{name}": {
        "get": {
          "tags": [
            "crate::resources::application"
          ],
          "operationId": "get_application",
          "parameters": [
            {
              "name": "namespace",
              "in": "path",
              "required": true,
              "type": "string"
            },
            {
              "name": "name",
              "in": "path",
              "required": true,
              "type": "string"
            }
          ],
          "responses": {},
          "x-kubernetes-group-version-kind": [
            {
              "group": "core.oam.dev",
              "kind": "Application",
              "version": "v1beta1"
            }
          ]
        },
        "delete": {
          "tags": [
            "crate::resources::application"
          ],
          "operationId": "delete_application",
          "parameters": [
            {
              "name": "namespace",
              "in": "path",
              "required": true,
              "type": "string"
            },
            {
              "name": "name",
              "in": "path",
              "required": true,
              "type": "string"
            }
          ],
          "responses": {},
          "x-kubernetes-group-version-kind": [
            {
              "group": "core.oam.dev",
              "kind": "Application",
              "version": "v1beta1"
            }
          ]
        },
        "patch": {
          "tags": [
            "crate::resources::application"
          ],
          "operationId": "patch_application",
          "parameters": [
            {
              "name": "namespace",
              "in": "path",
              "required": true,
              "type": "string"
            },
            {
              "name": "name",
              "in": "path",
              "required": true,
              "type": "string"
            },
            {
              "description": "",
              "required": true,
              "name": "body",
              "in": "body",
              "schema": {
                "type": "string",
                "format": "binary"
              }
            }
          ],
          "responses": {},
          "consumes": [
            "application/octet-stream"
          ],
          "x-kubernetes-group-version-kind": [
            {
              "group": "core.oam.dev",
              "kind": "Application",
              "version": "v1beta1"
            }
          ]
        }
      }
    },
    "swagger": "2.0",
    "definitions": {
      "dev.oam.core.v1beta1.ActorProperties": {
        "type": "object",
        "required": [
          "image"
        ],
        "properties": {
          "image": {
            "type": "string",
            "description": "The image reference to use"
          }
        },
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "ActorProperties",
            "version": "v1beta1"
          }
        ]
      },
      "dev.oam.core.v1beta1.Application": {
        "type": "object",
        "description": "An OAM manifest",
        "required": [
          "apiVersion",
          "kind",
          "metadata",
          "spec"
        ],
        "properties": {
          "apiVersion": {
            "type": "string",
            "description": "The OAM version of the manifest"
          },
          "kind": {
            "type": "string",
            "description": "The kind or type of manifest described by the spec"
          },
          "metadata": {
            "$ref": "#/definitions/dev.oam.core.v1beta1.Metadata"
          },
          "spec": {
            "$ref": "#/definitions/dev.oam.core.v1beta1.Specification"
          }
        },
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "Application",
            "version": "v1beta1"
          }
        ]
      },
      "dev.oam.core.v1beta1.CapabilityConfig": {
        "description": "Right now providers can technically use any config format they want, although most use JSON.\nThis enum takes that into account and allows either type of data to be passed",
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "CapabilityConfig",
            "version": "v1beta1"
          }
        ]
      },
      "dev.oam.core.v1beta1.CapabilityProperties": {
        "type": "object",
        "required": [
          "image",
          "contract"
        ],
        "properties": {
          "config": {
            "allOf": [
              {
                "$ref": "#/definitions/dev.oam.core.v1beta1.CapabilityConfig"
              }
            ],
            "x-nullable": true
          },
          "contract": {
            "type": "string",
            "description": "The contract ID of this capability"
          },
          "image": {
            "type": "string",
            "description": "The image reference to use"
          },
          "link_name": {
            "type": "string",
            "description": "An optional link name to use for this capability",
            "x-nullable": true
          }
        },
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "CapabilityProperties",
            "version": "v1beta1"
          }
        ]
      },
      "dev.oam.core.v1beta1.Component": {
        "allOf": [
          {
            "$ref": "#/definitions/dev.oam.core.v1beta1.Properties"
          },
          {
            "type": "object",
            "required": [
              "name"
            ],
            "properties": {
              "name": {
                "type": "string",
                "description": "The name of this component"
              },
              "traits": {
                "type": "array",
                "items": {
                  "$ref": "#/definitions/dev.oam.core.v1beta1.Trait"
                },
                "description": "A list of various traits assigned to this component",
                "x-nullable": true
              }
            }
          }
        ],
        "description": "A component definition",
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "Component",
            "version": "v1beta1"
          }
        ]
      },
      "dev.oam.core.v1beta1.LinkdefProperty": {
        "type": "object",
        "description": "Properties for linkdefs",
        "required": [
          "target"
        ],
        "properties": {
          "target": {
            "type": "string",
            "description": "The target this linkdef applies to. This should be the name of an actor component"
          },
          "values": {
            "type": "object",
            "description": "Values to use for this linkdef",
            "additionalProperties": {
              "type": "string"
            },
            "x-nullable": true
          }
        },
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "LinkdefProperty",
            "version": "v1beta1"
          }
        ]
      },
      "dev.oam.core.v1beta1.Metadata": {
        "type": "object",
        "description": "The metadata describing the manifest",
        "required": [
          "name"
        ],
        "properties": {
          "annotations": {
            "type": "object",
            "description": "Optional data for annotating this manifest",
            "additionalProperties": {
              "type": "string"
            }
          },
          "name": {
            "type": "string",
            "description": "The name of the manifest. This should be unique"
          },
          "namespace": {
            "type": "string",
            "description": "The namespace for the application."
          },
          "labels": {
            "type": "object",
            "description": "Optional data for labeling this manifest",
            "additionalProperties": {
              "type": "string"
            }
          }
        },
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "Metadata",
            "version": "v1beta1"
          }
        ]
      },
      "dev.oam.core.v1beta1.Properties": {
        "description": "Properties that can be defined for a component",
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "Properties",
            "version": "v1beta1"
          }
        ]
      },
      "dev.oam.core.v1beta1.Specification": {
        "type": "object",
        "description": "A representation of an OAM specification",
        "required": [
          "components"
        ],
        "properties": {
          "components": {
            "type": "array",
            "items": {
              "$ref": "#/definitions/dev.oam.core.v1beta1.Component"
            },
            "description": "The list of components for describing an application"
          }
        },
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "Specification",
            "version": "v1beta1"
          }
        ]
      },
      "dev.oam.core.v1beta1.Spread": {
        "type": "object",
        "description": "Configuration for various spreading requirements",
        "required": [
          "name"
        ],
        "properties": {
          "name": {
            "type": "string",
            "description": "The name of this spread requirement"
          },
          "requirements": {
            "type": "object",
            "description": "An arbitrary map of labels to match on for scaling requirements",
            "additionalProperties": {
              "type": "string"
            }
          },
          "weight": {
            "type": "integer",
            "description": "An optional weight for this spread. Higher weights are given more precedence",
            "minimum": 0,
            "x-nullable": true
          }
        },
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "Spread",
            "version": "v1beta1"
          }
        ]
      },
      "dev.oam.core.v1beta1.SpreadScalerProperty": {
        "type": "object",
        "description": "Properties for spread scalers",
        "required": [
          "replicas"
        ],
        "properties": {
          "replicas": {
            "type": "integer",
            "description": "Number of replicas to scale",
            "minimum": 0
          },
          "spread": {
            "type": "array",
            "items": {
              "$ref": "#/definitions/dev.oam.core.v1beta1.Spread"
            },
            "description": "Requirements for spreading throse replicas"
          }
        },
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "SpreadScalerProperty",
            "version": "v1beta1"
          }
        ]
      },
      "dev.oam.core.v1beta1.Trait": {
        "type": "object",
        "required": [
          "type",
          "properties"
        ],
        "properties": {
          "properties": {
            "$ref": "#/definitions/dev.oam.core.v1beta1.TraitProperty"
          },
          "type": {
            "type": "string",
            "description": "The type of trait specified. This should be a unique string for the type of scaler. As we\nplan on supporting custom scalers, these traits are not enumerated"
          }
        },
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "Trait",
            "version": "v1beta1"
          }
        ]
      },
      "dev.oam.core.v1beta1.TraitProperty": {
        "description": "Properties for defining traits",
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "TraitProperty",
            "version": "v1beta1"
          }
        ]
      }
    },
    "x-components": {}
}
"##;
