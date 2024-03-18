use std::fmt::Display;

use anyhow::{anyhow, Error};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
    Json, Router, TypedHeader,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{
    APIGroup, APIGroupList, APIResource, APIResourceList, GroupVersionForDiscovery, ObjectMeta,
    Status as StatusKind,
};
use kube::core::{GroupVersionKind, ListMeta};
use serde::Serialize;
use tracing::error;

use crate::{
    discovery::{
        APIGroupDiscovery, APIGroupDiscoveryList, APIResourceDiscovery, APIVersionDiscovery,
        DiscoveryFreshness, ResourceScope,
    },
    header::{Accept, As},
    openapi,
    resources::application::{
        create_application, delete_application, get_application, list_all_applications,
        list_applications, patch_application,
    },
    State,
};

pub fn setup(state: State) -> Router {
    let openapi_router = openapi::router();
    Router::new()
        .route("/apis/core.oam.dev/v1beta1", get(api_resources))
        .route(
            "/apis/core.oam.dev/v1beta1/applications",
            get(list_all_applications),
        )
        .route(
            "/apis/core.oam.dev/v1beta1/namespaces/:namespace/applications",
            get(list_applications),
        )
        .route(
            "/apis/core.oam.dev/v1beta1/namespaces/:namespace/applications",
            post(create_application),
        )
        .route(
            "/apis/core.oam.dev/v1beta1/namespaces/:namespace/applications/:name",
            get(get_application),
        )
        .route(
            "/apis/core.oam.dev/v1beta1/namespaces/:namespace/applications/:name",
            patch(patch_application),
        )
        .route(
            "/apis/core.oam.dev/v1beta1/namespaces/:namespace/applications/:name",
            delete(delete_application),
        )
        .with_state(state.clone())
        .route("/apis", get(api_groups))
        .route("/api", get(api_groups))
        .route("/health", get(health))
        .nest("/openapi", openapi_router)
}

#[utoipa::path(get, path = "/apis/core.oam.dev/v1beta1")]
async fn api_resources() -> Json<APIResourceList> {
    let resources = APIResourceList {
        group_version: "core.oam.dev/v1beta1".to_string(),
        resources: vec![APIResource {
            group: Some("core.oam.dev".to_string()),
            kind: "Application".to_string(),
            name: "applications".to_string(),
            namespaced: true,
            short_names: Some(vec!["app".to_string()]),
            singular_name: "application".to_string(),
            categories: Some(vec!["oam".to_string()]),
            verbs: vec![
                "create".to_string(),
                "get".to_string(),
                "list".to_string(),
                "patch".to_string(),
                "watch".to_string(),
            ],
            ..Default::default()
        }],
    };
    Json(resources)
}

async fn api_groups(TypedHeader(accept): TypedHeader<Accept>) -> impl IntoResponse {
    match accept.into() {
        // This is to support KEP-3352
        As::APIGroupDiscoveryList => Json(APIGroupDiscoveryList {
            items: vec![APIGroupDiscovery {
                metadata: ObjectMeta {
                    name: Some("core.oam.dev".to_string()),
                    ..Default::default()
                },
                versions: vec![APIVersionDiscovery {
                    version: "v1beta1".to_string(),
                    resources: vec![APIResourceDiscovery {
                        resource: "applications".to_string(),
                        response_kind: GroupVersionKind {
                            group: "core.oam.dev".to_string(),
                            version: "v1beta1".to_string(),
                            kind: "Application".to_string(),
                        },
                        scope: ResourceScope::Namespaced,
                        singular_resource: "application".to_string(),
                        verbs: vec![
                            "create".to_string(),
                            "get".to_string(),
                            "list".to_string(),
                            "patch".to_string(),
                            "watch".to_string(),
                        ],
                        short_names: vec!["app".to_string()],
                        categories: vec!["oam".to_string()],
                        // TODO(joonas): Add status resource here once we have support for it.
                        subresources: vec![],
                    }],
                    freshness: DiscoveryFreshness::Current,
                }],
                kind: None,
                api_version: None,
            }],
            ..Default::default()
        })
        .into_response(),
        // This is to support the "legacy" 'Accept: application/accept' requests.
        As::NotSpecified => Json(APIGroupList {
            groups: vec![APIGroup {
                name: "core.oam.dev".to_string(),
                preferred_version: Some(GroupVersionForDiscovery {
                    group_version: "core.oam.dev/v1beta1".to_string(),
                    version: "v1beta1".to_string(),
                }),
                versions: vec![GroupVersionForDiscovery {
                    group_version: "core.oam.dev/v1beta1".to_string(),
                    version: "v1beta1".to_string(),
                }],
                ..Default::default()
            }],
        })
        .into_response(),
        t => internal_error(anyhow!("unknown type request: {}", t)),
    }
}

async fn health() -> &'static str {
    "healthy"
}

/// PartialObjectMetadataList contains a list of objects containing only their metadata
// Based on https://github.com/kubernetes/kubernetes/blob/022d50fe3a1bdf2386395da7c266fede0c110040/staging/src/k8s.io/apimachinery/pkg/apis/meta/v1/types.go#L1469-L1480
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PartialObjectMetadataList {
    api_version: String,
    kind: String,
    // Standard list metadata.
    // More info: https://git.k8s.io/community/contributors/devel/sig-architecture/api-conventions.md#types-kinds
    metadata: ListMeta,
    // items contains each of the included items.
    items: Vec<PartialObjectMetadata>,
}

impl Default for PartialObjectMetadataList {
    fn default() -> Self {
        Self {
            api_version: "meta.k8s.io/v1".to_string(),
            kind: "PartialObjectMetadataList".to_string(),
            metadata: ListMeta::default(),
            items: vec![],
        }
    }
}

/// PartialObjectMetadata is a generic representation of any object with ObjectMeta. It allows clients
/// to get access to a particular ObjectMeta schema without knowing the details of the version.
// https://github.com/kubernetes/kubernetes/blob/022d50fe3a1bdf2386395da7c266fede0c110040/staging/src/k8s.io/apimachinery/pkg/apis/meta/v1/types.go#L1458-L1467
#[derive(Clone, Debug, Serialize)]
struct PartialObjectMetadata {
    api_version: String,
    kind: String,
    // Standard object's metadata.
    // More info: https://git.k8s.io/community/contributors/devel/sig-architecture/api-conventions.md#metadata
    metadata: ObjectMeta,
}

/// Helper for mapping any error into a `500 Internal Server Error` response.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn internal_error<E>(err: E) -> Response
where
    E: Into<Error> + Display,
{
    error!(%err);
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(StatusKind {
            code: Some(StatusCode::INTERNAL_SERVER_ERROR.as_u16() as i32),
            details: None,
            message: Some(err.to_string()),
            metadata: ListMeta::default(),
            reason: Some("StatusInternalServerError".to_string()),
            status: Some("Failure".to_string()),
        }),
    )
        .into_response()
}

/// Helper for mapping any error into a `404 Not Found` response.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn not_found_error<E>(err: E) -> Response
where
    E: Into<Error> + Display,
{
    error!(%err);
    (
        StatusCode::NOT_FOUND,
        Json(StatusKind {
            code: Some(StatusCode::NOT_FOUND.as_u16() as i32),
            details: None,
            message: Some(err.to_string()),
            metadata: ListMeta::default(),
            reason: Some("NotFound".to_string()),
            status: Some("Failure".to_string()),
        }),
    )
        .into_response()
}
