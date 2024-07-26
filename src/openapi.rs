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

    /// The list of policies describing an application. This is for providing application-wide
    /// setting such as configuration for a secrets backend, how to render Kubernetes services,
    /// etc. It can be omitted if no policies are needed for an application.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policies: Vec<Policy>,
}

/// A policy definition
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, ToSchema)]
#[schema(as = dev::oam::core::v1beta1::Policy)]
pub struct Policy {
    /// The name of this policy
    pub name: String,
    /// The properties for this policy
    pub properties: BTreeMap<String, String>,
    /// The type of the policy
    #[serde(rename = "type")]
    pub policy_type: String,
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
        ActorProperties,
        Application,
        CapabilityConfig,
        CapabilityProperties,
        Component,
        LinkdefProperty,
        Metadata,
        Policy,
        Properties,
        Specification,
        Spread,
        SpreadScalerProperty,
        Trait,
        TraitProperty,
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
  "swagger": "2.0",
  "info": {
    "title": "wasmcloud-operator",
    "description": "The OAM Application API provides a way to manage applications in a Kubernetes cluster.",
    "license": {
      "name": "Apache 2.0"
    },
    "version": "0.4.0"
  },
  "paths": {
    "/apis/core.oam.dev/v1beta1": {
      "get": {
        "parameters": [],
        "responses": {},
        "tags": [
          "crate::router"
        ],
        "operationId": "api_resources"
      }
    },
    "/apis/core.oam.dev/v1beta1/namespaces/{namespace}/applications": {
      "get": {
        "parameters": [
          {
            "in": "path",
            "name": "namespace",
            "required": true,
            "type": "string"
          }
        ],
        "responses": {},
        "tags": [
          "crate::resources::application"
        ],
        "operationId": "list_applications",
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "Application",
            "version": "v1beta1"
          }
        ]
      },
      "post": {
        "parameters": [
          {
            "in": "path",
            "name": "namespace",
            "required": true,
            "type": "string"
          },
          {
            "description": "",
            "in": "body",
            "name": "body",
            "required": true,
            "schema": {
              "type": "string",
              "format": "binary"
            }
          }
        ],
        "responses": {},
        "tags": [
          "crate::resources::application"
        ],
        "operationId": "create_application",
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
      "delete": {
        "parameters": [
          {
            "in": "path",
            "name": "namespace",
            "required": true,
            "type": "string"
          },
          {
            "in": "path",
            "name": "name",
            "required": true,
            "type": "string"
          }
        ],
        "responses": {},
        "tags": [
          "crate::resources::application"
        ],
        "operationId": "delete_application",
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "Application",
            "version": "v1beta1"
          }
        ]
      },
      "get": {
        "parameters": [
          {
            "in": "path",
            "name": "namespace",
            "required": true,
            "type": "string"
          },
          {
            "in": "path",
            "name": "name",
            "required": true,
            "type": "string"
          }
        ],
        "responses": {},
        "tags": [
          "crate::resources::application"
        ],
        "operationId": "get_application",
        "x-kubernetes-group-version-kind": [
          {
            "group": "core.oam.dev",
            "kind": "Application",
            "version": "v1beta1"
          }
        ]
      },
      "patch": {
        "parameters": [
          {
            "in": "path",
            "name": "namespace",
            "required": true,
            "type": "string"
          },
          {
            "in": "path",
            "name": "name",
            "required": true,
            "type": "string"
          },
          {
            "description": "",
            "in": "body",
            "name": "body",
            "required": true,
            "schema": {
              "type": "string",
              "format": "binary"
            }
          }
        ],
        "responses": {},
        "tags": [
          "crate::resources::application"
        ],
        "operationId": "patch_application",
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
  "definitions": {
    "dev.oam.core.v1beta1.ActorProperties": {
      "properties": {
        "image": {
          "description": "The image reference to use",
          "type": "string"
        }
      },
      "required": [
        "image"
      ],
      "type": "object",
      "x-kubernetes-group-version-kind": [
        {
          "group": "core.oam.dev",
          "kind": "ActorProperties",
          "version": "v1beta1"
        }
      ]
    },
    "dev.oam.core.v1beta1.Application": {
      "description": "An OAM manifest",
      "properties": {
        "apiVersion": {
          "description": "The OAM version of the manifest",
          "type": "string"
        },
        "kind": {
          "description": "The kind or type of manifest described by the spec",
          "type": "string"
        },
        "metadata": {
          "$ref": "#/definitions/dev.oam.core.v1beta1.Metadata"
        },
        "spec": {
          "$ref": "#/definitions/dev.oam.core.v1beta1.Specification"
        }
      },
      "required": [
        "apiVersion",
        "kind",
        "metadata",
        "spec"
      ],
      "type": "object",
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
      "oneOf": [
        {
          "properties": {
            "Json": {}
          },
          "required": [
            "Json"
          ],
          "type": "object"
        },
        {
          "properties": {
            "Opaque": {
              "type": "string"
            }
          },
          "required": [
            "Opaque"
          ],
          "type": "object"
        }
      ],
      "x-kubernetes-group-version-kind": [
        {
          "group": "core.oam.dev",
          "kind": "CapabilityConfig",
          "version": "v1beta1"
        }
      ]
    },
    "dev.oam.core.v1beta1.CapabilityProperties": {
      "properties": {
        "config": {
          "allOf": [
            {
              "$ref": "#/definitions/dev.oam.core.v1beta1.CapabilityConfig"
            }
          ],
          "nullable": true
        },
        "contract": {
          "description": "The contract ID of this capability",
          "type": "string"
        },
        "image": {
          "description": "The image reference to use",
          "type": "string"
        },
        "link_name": {
          "description": "An optional link name to use for this capability",
          "nullable": true,
          "type": "string"
        }
      },
      "required": [
        "image",
        "contract"
      ],
      "type": "object",
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
          "properties": {
            "name": {
              "description": "The name of this component",
              "type": "string"
            },
            "traits": {
              "description": "A list of various traits assigned to this component",
              "items": {
                "$ref": "#/definitions/dev.oam.core.v1beta1.Trait"
              },
              "nullable": true,
              "type": "array"
            }
          },
          "required": [
            "name"
          ],
          "type": "object"
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
      "description": "Properties for linkdefs",
      "properties": {
        "target": {
          "description": "The target this linkdef applies to. This should be the name of an actor component",
          "type": "string"
        },
        "values": {
          "additionalProperties": {
            "type": "string"
          },
          "description": "Values to use for this linkdef",
          "nullable": true,
          "type": "object"
        }
      },
      "required": [
        "target"
      ],
      "type": "object",
      "x-kubernetes-group-version-kind": [
        {
          "group": "core.oam.dev",
          "kind": "LinkdefProperty",
          "version": "v1beta1"
        }
      ]
    },
    "dev.oam.core.v1beta1.Metadata": {
      "description": "The metadata describing the manifest",
      "properties": {
        "annotations": {
          "additionalProperties": {
            "type": "string"
          },
          "description": "Optional data for annotating this manifest",
          "type": "object"
        },
        "labels": {
          "additionalProperties": {
            "type": "string"
          },
          "type": "object"
        },
        "name": {
          "description": "The name of the manifest. This should be unique",
          "type": "string"
        },
        "namespace": {
          "type": "string"
        }
      },
      "required": [
        "name",
        "namespace",
        "labels"
      ],
      "type": "object",
      "x-kubernetes-group-version-kind": [
        {
          "group": "core.oam.dev",
          "kind": "Metadata",
          "version": "v1beta1"
        }
      ]
    },
    "dev.oam.core.v1beta1.Policy": {
      "description": "A policy definition",
      "properties": {
        "name": {
          "description": "The name of this policy",
          "type": "string"
        },
        "properties": {
          "additionalProperties": {
            "type": "string"
          },
          "description": "The properties for this policy",
          "type": "object"
        },
        "type": {
          "description": "The type of the policy",
          "type": "string"
        }
      },
      "required": [
        "name",
        "properties",
        "type"
      ],
      "type": "object",
      "x-kubernetes-group-version-kind": [
        {
          "group": "core.oam.dev",
          "kind": "Policy",
          "version": "v1beta1"
        }
      ]
    },
    "dev.oam.core.v1beta1.Properties": {
      "description": "Properties that can be defined for a component",
      "discriminator": {
        "propertyName": "type"
      },
      "oneOf": [
        {
          "properties": {
            "properties": {
              "$ref": "#/definitions/dev.oam.core.v1beta1.ActorProperties"
            },
            "type": {
              "enum": [
                "actor"
              ],
              "type": "string"
            }
          },
          "required": [
            "properties",
            "type"
          ],
          "type": "object"
        },
        {
          "properties": {
            "properties": {
              "$ref": "#/definitions/dev.oam.core.v1beta1.CapabilityProperties"
            },
            "type": {
              "enum": [
                "capability"
              ],
              "type": "string"
            }
          },
          "required": [
            "properties",
            "type"
          ],
          "type": "object"
        }
      ],
      "x-kubernetes-group-version-kind": [
        {
          "group": "core.oam.dev",
          "kind": "Properties",
          "version": "v1beta1"
        }
      ]
    },
    "dev.oam.core.v1beta1.Specification": {
      "description": "A representation of an OAM specification",
      "properties": {
        "components": {
          "description": "The list of components for describing an application",
          "items": {
            "$ref": "#/definitions/dev.oam.core.v1beta1.Component"
          },
          "type": "array"
        },
        "policies": {
          "description": "The list of policies describing an application. This is for providing application-wide\nsetting such as configuration for a secrets backend, how to render Kubernetes services,\netc. It can be omitted if no policies are needed for an application.",
          "items": {
            "$ref": "#/definitions/dev.oam.core.v1beta1.Policy"
          },
          "type": "array"
        }
      },
      "required": [
        "components"
      ],
      "type": "object",
      "x-kubernetes-group-version-kind": [
        {
          "group": "core.oam.dev",
          "kind": "Specification",
          "version": "v1beta1"
        }
      ]
    },
    "dev.oam.core.v1beta1.Spread": {
      "description": "Configuration for various spreading requirements",
      "properties": {
        "name": {
          "description": "The name of this spread requirement",
          "type": "string"
        },
        "requirements": {
          "additionalProperties": {
            "type": "string"
          },
          "description": "An arbitrary map of labels to match on for scaling requirements",
          "type": "object"
        },
        "weight": {
          "description": "An optional weight for this spread. Higher weights are given more precedence",
          "minimum": 0,
          "nullable": true,
          "type": "integer"
        }
      },
      "required": [
        "name"
      ],
      "type": "object",
      "x-kubernetes-group-version-kind": [
        {
          "group": "core.oam.dev",
          "kind": "Spread",
          "version": "v1beta1"
        }
      ]
    },
    "dev.oam.core.v1beta1.SpreadScalerProperty": {
      "description": "Properties for spread scalers",
      "properties": {
        "replicas": {
          "description": "Number of replicas to scale",
          "minimum": 0,
          "type": "integer"
        },
        "spread": {
          "description": "Requirements for spreading throse replicas",
          "items": {
            "$ref": "#/definitions/dev.oam.core.v1beta1.Spread"
          },
          "type": "array"
        }
      },
      "required": [
        "replicas"
      ],
      "type": "object",
      "x-kubernetes-group-version-kind": [
        {
          "group": "core.oam.dev",
          "kind": "SpreadScalerProperty",
          "version": "v1beta1"
        }
      ]
    },
    "dev.oam.core.v1beta1.Trait": {
      "properties": {
        "properties": {
          "$ref": "#/definitions/dev.oam.core.v1beta1.TraitProperty"
        },
        "type": {
          "description": "The type of trait specified. This should be a unique string for the type of scaler. As we\nplan on supporting custom scalers, these traits are not enumerated",
          "type": "string"
        }
      },
      "required": [
        "type",
        "properties"
      ],
      "type": "object",
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
      "oneOf": [
        {
          "$ref": "#/definitions/dev.oam.core.v1beta1.LinkdefProperty"
        },
        {
          "$ref": "#/definitions/dev.oam.core.v1beta1.SpreadScalerProperty"
        },
        {}
      ],
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
