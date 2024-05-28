use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Error};
use async_nats::ConnectOptions;
use axum::{
    body::Bytes,
    extract::{Path, State as AxumState},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    TypedHeader,
};
use kube::{
    api::{Api, ListParams},
    client::Client,
    core::{ListMeta, ObjectMeta},
};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde_json::json;
use tokio::sync::RwLock;
use tracing::error;
use uuid::Uuid;
use wadm::{
    model::Manifest,
    server::{
        DeleteResult, DeployResult, GetResult, ModelSummary, PutResult, StatusResult, StatusType,
    },
};
use wasmcloud_operator_types::v1alpha1::WasmCloudHostConfig;

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

impl From<Vec<Manifest>> for ApplicationTable {
    fn from(manifests: Vec<Manifest>) -> Self {
        let mut table = Self::default();
        let rows = manifests
            .into_iter()
            .map(|m| TableRow {
                cells: vec![
                    m.metadata.name,
                    "N/A".to_string(),
                    match m.metadata.annotations.get("version") {
                        Some(v) => v.to_owned(),
                        None => "N/A".to_string(),
                    },
                    "N/A".to_string(),
                ],
            })
            .collect();

        table.rows = rows;
        table
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
    let client = match Client::try_default().await {
        Ok(c) => c,
        Err(e) => return internal_error(anyhow!("unable to initialize kubernetes client: {}", e)),
    };
    let configs: Api<WasmCloudHostConfig> = Api::namespaced(client, &namespace);
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

    let model: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return internal_error(anyhow!("unable to decode the patch: {}", e)),
    };

    let put =
        match wash_lib::app::put_model(&nats_client, Some(lattice_id.clone()), &model.to_string())
            .await
        {
            Ok(res) => res,
            Err(e) => return internal_error(anyhow!("could not deploy app: {}", e)),
        };

    let model_name = match put.result {
        PutResult::Created | PutResult::NewVersion => put.name,
        _ => {
            // TODO(joonas): Add handling for the case where the model version
            // might already exist (from prior deploy or otherwise).
            return internal_error(anyhow!(
                "unexpected response from wadm: result={:?}, message={}",
                put.result,
                put.message
            ));
        }
    };

    let deploy = match wash_lib::app::deploy_model(
        &nats_client,
        Some(lattice_id.clone()),
        &model_name,
        Some(put.current_version.clone()),
    )
    .await
    {
        Ok(res) => res,
        Err(e) => return internal_error(anyhow!("could not deploy app: {}", e)),
    };

    if deploy.result != DeployResult::Acknowledged {
        return internal_error(anyhow!(
            "unexpected response from wadm: result={:?}, message={}",
            deploy.result,
            deploy.message
        ));
    }

    // Get model from WADM for displaying in Kubernetes
    let get = match wash_lib::app::get_model_details(
        &nats_client,
        Some(lattice_id),
        &model_name,
        Some(put.current_version),
    )
    .await
    {
        Ok(res) => res,
        Err(e) => return internal_error(anyhow!("error getting deployed app: {}", e)),
    };

    match get.result {
        GetResult::Success => Json(Application::new(model_name)).into_response(),
        // Either we received an error or could not find the deployed application, so return an error:
        _ => internal_error(anyhow!(
            "unexpected response from wadm: result={:?}, message={}",
            get.result,
            get.message
        )),
    }
}

#[utoipa::path(get, path = "/apis/core.oam.dev/v1beta1/applications")]
pub async fn list_all_applications(
    TypedHeader(accept): TypedHeader<Accept>,
    AxumState(state): AxumState<State>,
) -> impl IntoResponse {
    // TODO(joonas): Use lattices (or perhaps Controller specific/special creds) for instanciating NATS client.
    // TODO(joonas): Add watch support to stop Argo from spamming this endpoint every second.

    let client = match Client::try_default().await {
        Ok(c) => c,
        Err(e) => return internal_error(anyhow!("unable to initialize kubernetes client: {}", e)),
    };

    let configs: Api<WasmCloudHostConfig> = Api::all(client);
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
                &cfg.spec.nats_address,
                &cfg.spec.nats_client_port,
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
    let client = match Client::try_default().await {
        Ok(c) => c,
        Err(e) => return internal_error(anyhow!("unable to initialize kubernetes client: {}", e)),
    };
    let configs: Api<WasmCloudHostConfig> = Api::namespaced(client, &namespace);
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
                &cfg.spec.nats_address,
                &cfg.spec.nats_client_port,
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
    let client = match creds {
        Some(creds) => {
            ConnectOptions::with_credentials(creds.expose_secret())?
                .connect(addr)
                .await?
        }
        None => ConnectOptions::new().connect(addr).await?,
    };
    let models = wash_lib::app::get_models(&client, Some(lattice_id)).await?;

    Ok(models)
}

pub async fn get_client(
    cluster_url: &str,
    port: &u16,
    nats_creds: Arc<RwLock<HashMap<NameNamespace, SecretString>>>,
    namespace: NameNamespace,
) -> Result<async_nats::Client, async_nats::ConnectError> {
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
    let client = match Client::try_default().await {
        Ok(c) => c,
        Err(e) => return internal_error(anyhow!("unable to initialize kubernetes client: {}", e)),
    };

    let configs: Api<WasmCloudHostConfig> = Api::namespaced(client, &namespace);
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

    let get =
        match wash_lib::app::get_model_details(&nats_client, Some(lattice_id.clone()), &name, None)
            .await
        {
            Ok(res) => res,
            Err(e) => {
                return internal_error(anyhow!("unable to request app from wadm: {}", e));
            }
        };

    let status = match wash_lib::app::get_model_status(
        &nats_client,
        Some(lattice_id.clone()),
        &name,
    )
    .await
    {
        Ok(res) => res,
        Err(e) => {
            return internal_error(anyhow!("unable to request app status from wadm: {}", e));
        }
    };

    match status.result {
        StatusResult::Ok => {}
        StatusResult::NotFound => {
            return not_found_error(anyhow!("applications \"{}\" not found", name));
        }
        StatusResult::Error => {
            return internal_error(anyhow!(
                "unexpected response from wadm: result={:?}, message={}",
                status.result,
                status.message
            ));
        }
    };

    if get.result == GetResult::Success {
        if let Some(manifest) = get.manifest {
            let response = match accept.into() {
                As::Table => Json(ApplicationTable::from(vec![manifest])).into_response(),
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
                    if let Some(status) = status.status {
                        let phase = match status.info.status_type {
                            StatusType::Undeployed => "Undeployed",
                            StatusType::Reconciling => "Reconciling",
                            StatusType::Deployed => "Deployed",
                            StatusType::Failed => "Failed",
                        };
                        manifest_value["status"] = json!({
                            "phase": phase,
                        });
                    }
                    Json(manifest_value).into_response()
                }
                // TODO(joonas): Add better error handling here
                t => return internal_error(anyhow!("unknown type: {}", t)),
            };
            return response;
        }
    };

    not_found_error(anyhow!("applications \"{}\" not found", name))
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
    let client = match Client::try_default().await {
        Ok(c) => c,
        Err(e) => return internal_error(anyhow!("unable to initialize kubernetes client: {}", e)),
    };
    let configs: Api<WasmCloudHostConfig> = Api::namespaced(client, &namespace);
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

    // Fist, check if the model exists.
    // TODO(joonas): we should likely fetch the version of the manifest that's running in Kubernetes
    // TODO(joonas): Should this use model.status instead of model.get?
    let get =
        match wash_lib::app::get_model_details(&nats_client, Some(lattice_id.clone()), &name, None)
            .await
        {
            Ok(res) => res,
            Err(e) => {
                return internal_error(anyhow!("unable to find app: {}", e));
            }
        };

    if get.result != GetResult::Success {
        return internal_error(anyhow!(
            "unexpected response from wadm: result={:?}, reason={}",
            get.result,
            get.message
        ));
    }

    // Prepare manifest for patching
    let Some(manifest) = get.manifest else {
        return internal_error(anyhow!("no manifest was found for app: {}", &name));
    };

    let mut model = serde_json::to_value(manifest).unwrap();
    // Parse the Kubernetes-provided RFC 7386 patch
    let patch: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => return internal_error(anyhow!("unable to decode the patch: {}", e)),
    };

    // Attempt to patch the currently running version
    json_patch::merge(&mut model, &patch);

    let put =
        match wash_lib::app::put_model(&nats_client, Some(lattice_id.clone()), &model.to_string())
            .await
        {
            Ok(res) => res,
            Err(e) => {
                return internal_error(anyhow!("could not update manifest for deploy: {}", e))
            }
        };

    match put.result {
        PutResult::NewVersion => {}
        _ => {
            // For now we have to check the error message to see if we can continue,
            // despite getting an error.
            if !put.message.contains("already exists") {
                return internal_error(anyhow!(
                    "unexpected response from wadm: result={:?}, message={}",
                    put.result,
                    put.message
                ));
            }
        }
    };

    let deploy = match wash_lib::app::deploy_model(
        &nats_client,
        Some(lattice_id),
        &name,
        Some(put.current_version),
    )
    .await
    {
        Ok(res) => res,
        Err(e) => return internal_error(anyhow!("could not deploy app: {}", e)),
    };

    match deploy.result {
        DeployResult::Acknowledged => Json(Application::new(name)).into_response(),
        DeployResult::Error => internal_error(anyhow!(
            "unexpected response from wadm: result={:?}, message={}",
            deploy.result,
            deploy.message
        )),
        DeployResult::NotFound => not_found_error(anyhow!("applications \"{}\" not found", &name)),
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
    let client = match Client::try_default().await {
        Ok(c) => c,
        Err(e) => return internal_error(anyhow!("unable to initialize kubernetes client: {}", e)),
    };

    let configs: Api<WasmCloudHostConfig> = Api::namespaced(client, &namespace);
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

    // Fist, check if the model exists.
    // TODO(joonas): Replace this with wash_lib::app::get_model_status once
    // https://github.com/wasmCloud/wasmCloud/pull/1151 ships.
    let status = match wash_lib::app::get_model_status(
        &nats_client,
        Some(lattice_id.clone()),
        &name,
    )
    .await
    {
        Ok(res) => res,
        Err(e) => {
            return internal_error(anyhow!("unable to request app status from wadm: {}", e));
        }
    };

    match status.result {
        StatusResult::Ok => {
            // Proceed
        }
        StatusResult::NotFound => {
            return not_found_error(anyhow!("apps \"{}\" not found", name));
        }
        StatusResult::Error => {
            return internal_error(anyhow!(
                "unexpected response from status command: result={:?}, message={}",
                status.result,
                status.message
            ));
        }
    };

    let delete = match wash_lib::app::delete_model_version(
        &nats_client,
        Some(lattice_id),
        &name,
        None,
        true,
    )
    .await
    {
        Ok(res) => res,
        Err(e) => return internal_error(anyhow!("error deleting app: {}", e)),
    };

    match delete.result {
        // TODO(joonas): do we need to handle DeleteResult::Noop differently?
        DeleteResult::Deleted | DeleteResult::Noop => Json(Application::new(name)).into_response(),
        DeleteResult::Error => internal_error(anyhow!(
            "unexpected response from wadm: result={:?}, message={}",
            delete.result,
            delete.message
        )),
    }
}

async fn get_lattice_connection(
    cfgs: impl Iterator<Item = WasmCloudHostConfig>,
    state: State,
    namespace: String,
) -> Result<(async_nats::Client, String), Response> {
    let connection_data =
        cfgs.map(|cfg| (cfg, namespace.clone()))
            .filter_map(|(cfg, namespace)| {
                let cluster_url = cfg.spec.nats_address;
                let lattice_id = cfg.spec.lattice;
                let lattice_name = cfg.metadata.name?;
                let nst: NameNamespace = NameNamespace::new(lattice_name, namespace);
                let port = cfg.spec.nats_client_port;
                Some((cluster_url, nst, lattice_id, port))
            });

    for (cluster_url, ns, lattice_id, port) in connection_data {
        match get_client(&cluster_url, &port, state.nats_creds.clone(), ns).await {
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
