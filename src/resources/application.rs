use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Error};
use async_nats::{Client as NatsClient, ConnectError, ConnectOptions};
use axum::{
    body::Bytes,
    extract::{Path, State as AxumState},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    TypedHeader,
};
use kube::{
    api::{Api, ListParams},
    client::Client as KubeClient,
    core::{ListMeta, ObjectMeta},
};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::error;
use uuid::Uuid;
use wadm_client::{error::ClientError, Client as WadmClient};
use wadm_types::{
    api::{ModelSummary, Status, StatusType},
    Manifest,
};

use wadm_operator_types::v1alpha1::WasmCloudHostConfig;

use crate::{
    controller::State,
    header::{Accept, As},
    router::{internal_error, not_found_error},
    table::{TableColumnDefinition, TableRow},
    NameNamespace,
};

/* TODO:
 * - Add a way to store Kubernetes **Namespace** the App belongs to.
 *   - Possibly using annotations that are set automatically when app is deployed into the cluster.
 * - Add a way to store app.kubernetes.io/name label and other Recommended labels: https://kubernetes.io/docs/concepts/overview/working-with-objects/common-labels/
 *  - Current hack results in a "SharedResourceWarning" warning in the UI: https://github.com/argoproj/argo-cd/blob/a761a495f16d76c0a8e50359eda50f605e329aba/controller/state.go#L529-L537
 * - Add a way to support all of the Argo resource tracking methods: https://argo-cd.readthedocs.io/en/stable/user-guide/resource_tracking/
 */

const GROUP_VERSION: &str = "core.oam.dev/v1beta1";
const KUBECTL_LAST_APPLIED_CONFIG_ANNOTATION: &str =
    "kubectl.kubernetes.io/last-applied-configuration";

pub struct AppError(Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Error: {}", self.0),
        )
            .into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<Error>,
{
    fn from(e: E) -> Self {
        Self(e.into())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Application {
    pub api_version: String,
    pub kind: String,
    pub metadata: ObjectMeta,
}

impl Application {
    pub fn new(name: String) -> Self {
        Self {
            api_version: "v1beta1".to_string(),
            kind: "Application".to_string(),
            metadata: ObjectMeta {
                name: Some(name),
                ..Default::default()
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationList {
    api_version: String,
    kind: String,
    items: Vec<ApplicationPartial>,
    metadata: ListMeta,
}

impl ApplicationList {
    pub fn new() -> Self {
        Self {
            api_version: GROUP_VERSION.to_string(),
            kind: "ApplicationList".to_string(),
            items: vec![],
            metadata: ListMeta::default(),
        }
    }
}

impl Default for ApplicationList {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Vec<ApplicationPartial>> for ApplicationList {
    fn from(partials: Vec<ApplicationPartial>) -> Self {
        let mut al = ApplicationList::default();
        // TODO(joonas): Let's figure out a better way to do this shall we?
        let v = serde_json::to_value(&partials).unwrap();
        let resource_version = Uuid::new_v5(&Uuid::NAMESPACE_OID, v.to_string().as_bytes());
        al.metadata.resource_version = Some(resource_version.to_string());
        al.items = partials;
        al
    }
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationPartial {
    metadata: ObjectMeta,
    spec: BTreeMap<String, String>,
    status: BTreeMap<String, String>,
}

impl From<ModelSummary> for ApplicationPartial {
    fn from(summary: ModelSummary) -> Self {
        let ns = format!("{}/{}", summary.name, summary.version);
        let uid = Uuid::new_v5(&Uuid::NAMESPACE_OID, ns.as_bytes());
        Self {
            metadata: ObjectMeta {
                name: Some(summary.name),
                // TODO(joonas): Infer this, or make it something that can be set later.
                namespace: Some("default".to_string()),
                resource_version: Some(uid.to_string()),
                uid: Some(uid.to_string()),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

// Definition: https://pkg.go.dev/k8s.io/apimachinery/pkg/apis/meta/v1#Table
// Based on https://github.com/kubernetes-sigs/metrics-server/blob/master/pkg/api/table.go
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationTable {
    api_version: String,
    kind: String,
    column_definitions: Vec<TableColumnDefinition>,
    rows: Vec<TableRow>,
    metadata: ListMeta,
}

impl Default for ApplicationTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ApplicationTable {
    pub fn new() -> Self {
        Self {
            api_version: "meta.k8s.io/v1".to_string(),
            kind: "Table".to_string(),
            column_definitions: vec![
                TableColumnDefinition {
                    name: "Application".to_string(),
                    kind: "string".to_string(),
                    description: "Name of the Application".to_string(),
                    priority: 0,
                    format: "name".to_string(),
                },
                TableColumnDefinition {
                    name: "Deployed Version".to_string(),
                    kind: "string".to_string(),
                    description: "Currently deployed version of the Application".to_string(),
                    priority: 0,
                    ..Default::default()
                },
                TableColumnDefinition {
                    name: "Latest Version".to_string(),
                    kind: "string".to_string(),
                    description: "Latest available version of the Application".to_string(),
                    priority: 0,
                    ..Default::default()
                },
                TableColumnDefinition {
                    name: "Status".to_string(),
                    kind: "string".to_string(),
                    description: "Current status of the Application".to_string(),
                    priority: 0,
                    ..Default::default()
                },
            ],
            rows: vec![],
            metadata: ListMeta::default(),
        }
    }
}

impl From<Vec<ModelSummary>> for ApplicationTable {
    fn from(summaries: Vec<ModelSummary>) -> Self {
        let mut table = Self::default();
        let rows = summaries
            .into_iter()
            .map(|i| TableRow {
                cells: vec![
                    i.name,
                    i.deployed_version.unwrap_or("N/A".to_string()),
                    i.version,
                    match i.status {
                        StatusType::Undeployed => "Undeployed".to_string(),
                        StatusType::Reconciling => "Reconciling".to_string(),
                        StatusType::Deployed => "Deployed".to_string(),
                        StatusType::Failed => "Failed".to_string(),
                    },
                ],
            })
            .collect();

        table.rows = rows;
        table
    }
}

impl From<Vec<CombinedManifest>> for ApplicationTable {
    fn from(manifests: Vec<CombinedManifest>) -> Self {
        let mut table = Self::default();
        let rows = manifests
            .into_iter()
            .map(|cm| TableRow {
                cells: vec![
                    cm.name(),
                    cm.deployed_version(),
                    cm.latest_version(),
                    cm.status(),
                ],
            })
            .collect();

        table.rows = rows;
        table
    }
}

struct CombinedManifest {
    manifest: Manifest,
    status: Status,
}

impl CombinedManifest {
    pub(crate) fn new(manifest: Manifest, status: Status) -> Self {
        Self { manifest, status }
    }

    pub(crate) fn name(&self) -> String {
        self.manifest.metadata.name.to_owned()
    }

    pub(crate) fn deployed_version(&self) -> String {
        match self.manifest.metadata.annotations.get("version") {
            Some(v) => v.to_owned(),
            None => "N/A".to_string(),
        }
    }

    pub(crate) fn latest_version(&self) -> String {
        self.status.version.to_owned()
    }

    pub(crate) fn status(&self) -> String {
        match self.status.info.status_type {
            StatusType::Undeployed => "Undeployed",
            StatusType::Reconciling => "Reconciling",
            StatusType::Deployed => "Deployed",
            StatusType::Failed => "Failed",
        }
        .to_string()
    }
}

#[utoipa::path(
    post,
    path = "/apis/core.oam.dev/v1beta1/namespaces/{namespace}/applications"
)]
pub async fn create_application(
    Path(namespace): Path<String>,
    AxumState(state): AxumState<State>,
    body: Bytes,
) -> impl IntoResponse {
    let kube_client = match KubeClient::try_default().await {
        Ok(c) => c,
        Err(e) => return internal_error(anyhow!("unable to initialize kubernetes client: {}", e)),
    };
    let configs: Api<WasmCloudHostConfig> = Api::namespaced(kube_client, &namespace);
    let cfgs = match configs.list(&ListParams::default()).await {
        Ok(objs) => objs,
        Err(e) => return internal_error(anyhow!("Unable to list cosmonic host configs: {}", e)),
    };

    // TODO(joonas): Remove this once we move to pulling NATS creds+secrets from lattice instead of hosts.
    let (nats_client, lattice_id) =
        match get_lattice_connection(cfgs.into_iter(), state, namespace).await {
            Ok(data) => data,
            Err(resp) => return resp,
        };

    let wadm_client = WadmClient::from_nats_client(&lattice_id, None, nats_client);

    let manifest: Manifest = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return internal_error(anyhow!("unable to decode the patch: {}", e)),
    };

    let (application_name, _application_version) =
        match wadm_client.put_and_deploy_manifest(manifest).await {
            Ok(application_bits) => application_bits,
            Err(e) => return internal_error(anyhow!("could not deploy app: {}", e)),
        };

    Json(Application::new(application_name)).into_response()
}

#[utoipa::path(get, path = "/apis/core.oam.dev/v1beta1/applications")]
pub async fn list_all_applications(
    TypedHeader(accept): TypedHeader<Accept>,
    AxumState(state): AxumState<State>,
) -> impl IntoResponse {
    // TODO(joonas): Use lattices (or perhaps Controller specific/special creds) for instanciating NATS client.
    // TODO(joonas): Add watch support to stop Argo from spamming this endpoint every second.

    let kube_client = match KubeClient::try_default().await {
        Ok(c) => c,
        Err(e) => return internal_error(anyhow!("unable to initialize kubernetes client: {}", e)),
    };

    let configs: Api<WasmCloudHostConfig> = Api::all(kube_client);
    let cfgs = match configs.list(&ListParams::default()).await {
        Ok(objs) => objs,
        Err(e) => return internal_error(anyhow!("Unable to list cosmonic host configs: {}", e)),
    };

    let mut apps = Vec::new();
    let mut lattices = HashSet::new();
    for cfg in cfgs {
        let name = cfg.metadata.name.unwrap().clone();
        let lattice_id = cfg.spec.lattice.clone();
        let namespace = cfg.metadata.namespace.unwrap().clone();
        let nst = NameNamespace::new(name, namespace.clone());
        let map = state.nats_creds.read().await;
        let secret = map.get(&nst);
        // Prevent listing applications within a given lattice more than once
        if !lattices.contains(&lattice_id) {
            let result = match list_apps(
                &cfg.spec.effective_nats_address(),
                &cfg.spec.effective_nats_client_port(),
                secret,
                lattice_id.clone(),
            )
            .await
            {
                Ok(apps) => apps,
                Err(e) => return internal_error(anyhow!("unable to list applications: {}", e)),
            };
            apps.extend(result);
            lattices.insert(lattice_id);
        }
    }

    // We're trying to match the appopriate response based on what Kubernetes/kubectl asked for.
    match accept.into() {
        As::Table => Json(ApplicationTable::from(apps)).into_response(),
        As::NotSpecified => {
            let partials: Vec<ApplicationPartial> = apps
                .iter()
                .map(|a| ApplicationPartial::from(a.clone()))
                .collect();

            Json(ApplicationList::from(partials)).into_response()
        }
        // TODO(joonas): Add better error handling here
        _ => Json("").into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/apis/core.oam.dev/v1beta1/namespaces/{namespace}/applications"
)]
pub async fn list_applications(
    TypedHeader(accept): TypedHeader<Accept>,
    Path(namespace): Path<String>,
    AxumState(state): AxumState<State>,
) -> impl IntoResponse {
    let kube_client = match KubeClient::try_default().await {
        Ok(c) => c,
        Err(e) => return internal_error(anyhow!("unable to initialize kubernetes client: {}", e)),
    };
    let configs: Api<WasmCloudHostConfig> = Api::namespaced(kube_client, &namespace);
    let cfgs = match configs.list(&ListParams::default()).await {
        Ok(objs) => objs,
        Err(e) => return internal_error(anyhow!("Unable to list cosmonic host configs: {}", e)),
    };

    let mut apps = Vec::new();
    let mut lattices = HashSet::new();
    for cfg in cfgs {
        let name = cfg.metadata.name.unwrap().clone();
        let lattice_id = cfg.spec.lattice.clone();
        let nst = NameNamespace::new(name, namespace.clone());
        let map = state.nats_creds.read().await;
        let secret = map.get(&nst);
        // This is to check that we don't list a lattice more than once
        if !lattices.contains(&lattice_id) {
            let result = match list_apps(
                &cfg.spec.effective_nats_address(),
                &cfg.spec.effective_nats_client_port(),
                secret,
                lattice_id.clone(),
            )
            .await
            {
                Ok(apps) => apps,
                Err(e) => return internal_error(anyhow!("unable to list applications: {}", e)),
            };
            apps.extend(result);
            lattices.insert(lattice_id);
        }
    }

    // We're trying to match the appopriate response based on what Kubernetes/kubectl asked for.
    match accept.into() {
        As::Table => Json(ApplicationTable::from(apps)).into_response(),
        As::NotSpecified => {
            let partials: Vec<ApplicationPartial> = apps
                .iter()
                .map(|a| ApplicationPartial::from(a.clone()))
                .collect();
            Json(ApplicationList::from(partials)).into_response()
        }
        // TODO(joonas): Add better error handling here
        _ => Json("").into_response(),
    }
}

pub async fn list_apps(
    cluster_url: &str,
    port: &u16,
    creds: Option<&SecretString>,
    lattice_id: String,
) -> Result<Vec<ModelSummary>, Error> {
    let addr = format!("{}:{}", cluster_url, port);
    let nats_client = match creds {
        Some(creds) => {
            ConnectOptions::with_credentials(creds.expose_secret())?
                .connect(addr)
                .await?
        }
        None => ConnectOptions::new().connect(addr).await?,
    };
    let wadm_client = WadmClient::from_nats_client(&lattice_id, None, nats_client);
    Ok(wadm_client.list_manifests().await?)
}

pub async fn get_nats_client(
    cluster_url: &str,
    port: &u16,
    nats_creds: Arc<RwLock<HashMap<NameNamespace, SecretString>>>,
    namespace: NameNamespace,
) -> Result<NatsClient, ConnectError> {
    let addr = format!("{}:{}", cluster_url, port);
    let creds = nats_creds.read().await;
    match creds.get(&namespace) {
        Some(creds) => {
            let creds = creds.expose_secret();
            ConnectOptions::with_credentials(creds)
                .expect("unable to create nats client")
                .connect(addr)
                .await
        }
        None => ConnectOptions::new().connect(addr).await,
    }
}

#[utoipa::path(
    get,
    path = "/apis/core.oam.dev/v1beta1/namespaces/{namespace}/applications/{name}"
)]
pub async fn get_application(
    TypedHeader(accept): TypedHeader<Accept>,
    Path((namespace, name)): Path<(String, String)>,
    AxumState(state): AxumState<State>,
) -> impl IntoResponse {
    let kube_client = match KubeClient::try_default().await {
        Ok(c) => c,
        Err(e) => return internal_error(anyhow!("unable to initialize kubernetes client: {}", e)),
    };

    let configs: Api<WasmCloudHostConfig> = Api::namespaced(kube_client, &namespace);
    let cfgs = match configs.list(&ListParams::default()).await {
        Ok(objs) => objs,
        Err(e) => return internal_error(anyhow!("unable to list cosmonic host configs: {}", e)),
    };

    // TODO(joonas): Remove this once we move to pulling NATS creds+secrets from lattice instead of hosts.
    let (nats_client, lattice_id) =
        match get_lattice_connection(cfgs.into_iter(), state, namespace.clone()).await {
            Ok(data) => data,
            Err(resp) => return resp,
        };
    let wadm_client = WadmClient::from_nats_client(&lattice_id, None, nats_client);

    let manifest = match wadm_client.get_manifest(&name, None).await {
        Ok(m) => m,
        Err(e) => match e {
            ClientError::NotFound(_) => {
                return not_found_error(anyhow!("applications \"{}\" not found", name))
            }
            _ => return internal_error(anyhow!("unable to request app from wadm: {}", e)),
        },
    };
    let status = match wadm_client.get_manifest_status(&name).await {
        Ok(s) => s,
        Err(e) => match e {
            ClientError::NotFound(_) => {
                return not_found_error(anyhow!("applications \"{}\" not found", name))
            }
            _ => return internal_error(anyhow!("unable to request app status from wadm: {}", e)),
        },
    };

    match accept.into() {
        As::Table => {
            let combined_manifest = CombinedManifest::new(manifest, status);
            Json(ApplicationTable::from(vec![combined_manifest])).into_response()
        }
        As::NotSpecified => {
            // TODO(joonas): This is a terrible hack, but for now it's what we need to do to satisfy Argo/Kubernetes since WADM doesn't support this metadata.
            let mut manifest_value = serde_json::to_value(&manifest).unwrap();
            // TODO(joonas): We should add lattice id to this as well, but we need it in every place where the application is listed.
            let ns = format!("{}/{}", &name, &manifest.version());
            let uid = Uuid::new_v5(&Uuid::NAMESPACE_OID, ns.as_bytes());
            manifest_value["metadata"]["uid"] = json!(uid.to_string());
            manifest_value["metadata"]["resourceVersion"] = json!(uid.to_string());
            manifest_value["metadata"]["namespace"] = json!(namespace);
            manifest_value["metadata"]["labels"] = json!({
                "app.kubernetes.io/instance": &name
            });
            // TODO(joonas): refactor status and the metadata inputs into a struct we could just serialize
            // The custom health check we provide for Argo will handle the case where status is missing, so this is fine for now.
            let phase = match status.info.status_type {
                StatusType::Undeployed => "Undeployed",
                StatusType::Reconciling => "Reconciling",
                StatusType::Deployed => "Deployed",
                StatusType::Failed => "Failed",
            };
            manifest_value["status"] = json!({
                "phase": phase,
            });
            Json(manifest_value).into_response()
        }
        // TODO(joonas): Add better error handling here
        t => internal_error(anyhow!("unknown type: {}", t)),
    }
}

#[utoipa::path(
    patch,
    path = "/apis/core.oam.dev/v1beta1/namespaces/{namespace}/applications/{name}"
)]
pub async fn patch_application(
    Path((namespace, name)): Path<(String, String)>,
    AxumState(state): AxumState<State>,
    body: Bytes,
) -> impl IntoResponse {
    let kube_client = match KubeClient::try_default().await {
        Ok(c) => c,
        Err(e) => return internal_error(anyhow!("unable to initialize kubernetes client: {}", e)),
    };
    let configs: Api<WasmCloudHostConfig> = Api::namespaced(kube_client, &namespace);
    let cfgs = match configs.list(&ListParams::default()).await {
        Ok(objs) => objs,
        Err(e) => return internal_error(anyhow!("unable to list cosmonic host configs: {}", e)),
    };

    // TODO(joonas): Remove this once we move to pulling NATS creds+secrets from lattice instead of hosts.
    let (nats_client, lattice_id) =
        match get_lattice_connection(cfgs.into_iter(), state, namespace).await {
            Ok(data) => data,
            Err(resp) => return resp,
        };
    let wadm_client = WadmClient::from_nats_client(&lattice_id, None, nats_client);
    let current_manifest = match wadm_client.get_manifest(&name, None).await {
        Ok(m) => m,
        Err(e) => match e {
            ClientError::NotFound(_) => {
                return not_found_error(anyhow!("applications \"{}\" not found", name))
            }
            _ => return internal_error(anyhow!("unable to request app from wadm: {}", e)),
        },
    };

    let mut current = serde_json::to_value(current_manifest).unwrap();
    // Parse the Kubernetes-provided RFC 7386 patch
    let patch = match serde_json::from_slice::<Value>(&body) {
        Ok(p) => p,
        Err(e) => return internal_error(anyhow!("unable to decode the patch: {}", e)),
    };

    // Remove kubectl.kubernetes.io/last-applied-configuration annotation before
    // we compare against the patch, otherwise we'll always end up creating a new version.
    let last_applied_configuration = current
        .get_mut("metadata")
        .and_then(|metadata| metadata.get_mut("annotations"))
        .and_then(|annotations| annotations.as_object_mut())
        .and_then(|annotations| annotations.remove(KUBECTL_LAST_APPLIED_CONFIG_ANNOTATION));

    // TODO(joonas): This doesn't quite work as intended at the moment,
    // there are some differences in terms like replicas vs. instances:
    // * Add(AddOperation { path: "/spec/components/0/traits/0/properties/replicas", value: Number(1) }),
    // * Remove(RemoveOperation { path: "/spec/components/0/traits/0/properties/instances" }),
    //
    // which cause the server to always patch. Also, top-level entries such
    // as apiVersion, kind and metadata are always removed.
    //
    // let diff = json_patch::diff(&current, &patch);
    // if diff.is_empty() {
    //     // If there's nothing to patch, return early.
    //     return Json(()).into_response();
    // };

    // Remove current version so that either a new version is generated,
    // or the one set in the incoming patch gets used.
    if let Some(annotations) = current
        .get_mut("metadata")
        .and_then(|metadata| metadata.get_mut("annotations"))
        .and_then(|annotations| annotations.as_object_mut())
    {
        annotations.remove("version");
    }

    // Attempt to patch the currently running version
    json_patch::merge(&mut current, &patch);

    // Re-insert "kubectl.kubernetes.io/last-applied-configuration" if one was set
    if let Some(last_applied_config) = last_applied_configuration {
        if let Some(annotations) = current
            .get_mut("metadata")
            .and_then(|metadata| metadata.get_mut("annotations"))
            .and_then(|annotations| annotations.as_object_mut())
        {
            annotations.insert(
                KUBECTL_LAST_APPLIED_CONFIG_ANNOTATION.to_string(),
                last_applied_config,
            );
        }
    }

    let updated_manifest = match serde_json::from_value::<Manifest>(current) {
        Ok(m) => m,
        Err(e) => return internal_error(anyhow!("unable to patch the application: {}", e)),
    };

    match wadm_client.put_and_deploy_manifest(updated_manifest).await {
        Ok((app_name, _)) => Json(Application::new(app_name)).into_response(),
        Err(e) => match e {
            ClientError::NotFound(_) => {
                not_found_error(anyhow!("applications \"{}\" not found", &name))
            }
            _ => internal_error(anyhow!("could not update application: {}", e)),
        },
    }
}

#[utoipa::path(
    delete,
    path = "/apis/core.oam.dev/v1beta1/namespaces/{namespace}/applications/{name}"
)]
pub async fn delete_application(
    Path((namespace, name)): Path<(String, String)>,
    AxumState(state): AxumState<State>,
) -> impl IntoResponse {
    let kube_client = match KubeClient::try_default().await {
        Ok(c) => c,
        Err(e) => return internal_error(anyhow!("unable to initialize kubernetes client: {}", e)),
    };

    let configs: Api<WasmCloudHostConfig> = Api::namespaced(kube_client, &namespace);
    let cfgs = match configs.list(&ListParams::default()).await {
        Ok(objs) => objs,
        Err(e) => return internal_error(anyhow!("unable to list cosmonic host configs: {}", e)),
    };

    // TODO(joonas): Remove this once we move to pulling NATS creds+secrets from lattice instead of hosts.
    let (nats_client, lattice_id) =
        match get_lattice_connection(cfgs.into_iter(), state, namespace).await {
            Ok(data) => data,
            Err(resp) => return resp,
        };

    let wadm_client = WadmClient::from_nats_client(&lattice_id, None, nats_client);
    match wadm_client.delete_manifest(&name, None).await {
        Ok(_) => Json(Application::new(name)).into_response(),
        Err(e) => match e {
            ClientError::NotFound(_) => not_found_error(anyhow!("apps \"{}\" not found", name)),
            _ => internal_error(anyhow!("could not delete app: {}", e)),
        },
    }
}

async fn get_lattice_connection(
    cfgs: impl Iterator<Item = WasmCloudHostConfig>,
    state: State,
    namespace: String,
) -> Result<(NatsClient, String), Response> {
    let connection_data =
        cfgs.map(|cfg| (cfg, namespace.clone()))
            .filter_map(|(cfg, namespace)| {
                let cluster_url = cfg.spec.effective_nats_address();
                let lattice_id = cfg.spec.lattice.clone();
                let lattice_name = cfg.metadata.name?;
                let nst: NameNamespace = NameNamespace::new(lattice_name, namespace);
                let port = cfg.spec.effective_nats_client_port();
                Some((cluster_url, nst, lattice_id, port))
            });

    for (cluster_url, ns, lattice_id, port) in connection_data {
        match get_nats_client(&cluster_url, &port, state.nats_creds.clone(), ns).await {
            Ok(c) => return Ok((c, lattice_id)),
            Err(e) => {
                error!(err = %e, %lattice_id, "error connecting to nats");
                continue;
            }
        };
    }

    // If we get here, we couldn't get a NATS client, so return an error
    Err(internal_error(anyhow!("unable to initialize nats client")))
}
