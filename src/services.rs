use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use async_nats::{
    jetstream,
    jetstream::{
        consumer::{pull::Config, Consumer},
        stream::{Config as StreamConfig, RetentionPolicy, Source, StorageType, SubjectTransform},
        AckKind,
    },
    Client,
};
use cloudevents::{AttributesReader, Event as CloudEvent};
use futures::StreamExt;
use k8s_openapi::api::core::v1::{Pod, Service, ServicePort, ServiceSpec};
use k8s_openapi::api::discovery::v1::{Endpoint, EndpointConditions, EndpointPort, EndpointSlice};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::{
    api::{Api, DeleteParams, ListParams, Patch, PatchParams},
    client::Client as KubeClient,
    Resource,
};
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};
use wadm::events::{Event, ManifestPublished, ManifestUnpublished};
use wadm_client::Client as WadmClient;
use wadm_types::{api::ModelSummary, Component, Manifest, Properties, Trait, TraitProperty};
use wasmcloud_operator_types::v1alpha1::WasmCloudHostConfig;

use crate::controller::{
    common_labels, CLUSTER_CONFIG_FINALIZER, SERVICE_FINALIZER,
    WASMCLOUD_OPERATOR_HOST_LABEL_PREFIX, WASMCLOUD_OPERATOR_MANAGED_BY_LABEL_REQUIREMENT,
};

const CONSUMER_PREFIX: &str = "wasmcloud_operator_service";
// This should probably be exposed by wadm somewhere
const WADM_EVENT_STREAM_NAME: &str = "wadm_events";
const OPERATOR_STREAM_NAME: &str = "wasmcloud_operator_events";
const OPERATOR_STREAM_SUBJECT: &str = "wasmcloud_operator_events.*.>";

/// Commands that can be sent to the watcher to trigger an update or removal of a service.
#[derive(Clone, Debug)]
enum WatcherCommand {
    UpsertService(ServiceParams),
    RemoveService {
        name: String,
        namespaces: HashSet<String>,
    },
    RemoveServices {
        namespaces: HashSet<String>,
    },
}

/// Parameters for creating or updating a service in the cluster.
#[derive(Clone, Debug)]
pub struct ServiceParams {
    name: String,
    namespaces: HashSet<String>,
    lattice_id: String,
    port: u16,
    host_labels: Option<HashMap<String, String>>,
}

/// Watches for new services to be created in the cluster for a partcular lattice and creates or
/// updates them as necessary.
#[derive(Clone, Debug)]
pub struct Watcher {
    namespaces: HashSet<String>,
    lattice_id: String,
    nats_client: Client,
    shutdown: CancellationToken,
    consumer: Consumer<Config>,
    tx: mpsc::UnboundedSender<WatcherCommand>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl Watcher {
    /// Creates a new watcher for a particular lattice.
    fn new(
        namespace: String,
        lattice_id: String,
        nats_client: Client,
        consumer: Consumer<Config>,
        tx: mpsc::UnboundedSender<WatcherCommand>,
    ) -> Self {
        let watcher = Self {
            namespaces: HashSet::from([namespace]),
            nats_client,
            lattice_id: lattice_id.clone(),
            consumer,
            shutdown: CancellationToken::new(),
            tx,
        };

        // TODO is there a better way to handle this?
        let watcher_dup = watcher.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = watcher_dup.shutdown.cancelled() => {
                    debug!(%lattice_id, "Service watcher shutting down for lattice");
                }
                _ = watcher_dup.watch_events(&watcher_dup.consumer) => {
                    error!(%lattice_id, "Service watcher for lattice has stopped");
                }
            }
        });

        watcher
    }

    /// Watches for new events on the mirrored wadm_events stream and processes them.
    async fn watch_events(&self, consumer: &Consumer<Config>) -> Result<()> {
        let mut messages = consumer.stream().messages().await?;
        while let Some(message) = messages.next().await {
            if let Ok(message) = message {
                match self.handle_event(message.clone()) {
                    Ok(_) => message
                        .ack()
                        .await
                        .map_err(|e| {
                            error!(err=%e, "Error acking message");
                            e
                        })
                        .ok(),
                    Err(_) => message
                        .ack_with(AckKind::Nak(None))
                        .await
                        .map_err(|e| {
                            error!(err=%e, "Error nacking message");
                            e
                        })
                        .ok(),
                };
            }
        }
        Ok(())
    }

    /// Handles a new event from the consumer.
    fn handle_event(&self, message: async_nats::jetstream::Message) -> Result<()> {
        let event = serde_json::from_slice::<CloudEvent>(&message.payload)
            .map_err(|e| anyhow::anyhow!("Error parsing cloudevent: {}", e))?;
        let evt = match Event::try_from(event.clone()) {
            Ok(evt) => evt,
            Err(e) => {
                warn!(
                    err=%e,
                    event_type=%event.ty(),
                    "Error converting cloudevent to wadm event",
                );
                return Ok(());
            }
        };
        match evt {
            Event::ManifestPublished(mp) => {
                let name = mp.manifest.metadata.name.clone();
                self.handle_manifest_published(mp).map_err(|e| {
                    error!(lattice_id = %self.lattice_id, manifest = name, "Error handling manifest published event: {}", e);
                        e
                })?;
            }
            Event::ManifestUnpublished(mu) => {
                let name = mu.name.clone();
                self.handle_manifest_unpublished(mu).map_err(|e| {
                    error!(lattice_id = %self.lattice_id, manifest = name, "Error handling manifest unpublished event: {}", e);
                    e
                })?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Handles a manifest published event.
    fn handle_manifest_published(&self, mp: ManifestPublished) -> Result<()> {
        debug!(manifest=?mp, "Handling manifest published event");
        let manifest = mp.manifest;
        if let Some(httpserver_service) = http_server_component(&manifest) {
            if let Ok(addr) = httpserver_service.address.parse::<SocketAddr>() {
                debug!(manifest = %manifest.metadata.name, "Upserting service for manifest");
                self.tx
                    .send(WatcherCommand::UpsertService(ServiceParams {
                        name: manifest.metadata.name.clone(),
                        lattice_id: self.lattice_id.clone(),
                        port: addr.port(),
                        namespaces: self.namespaces.clone(),
                        host_labels: httpserver_service.labels,
                    }))
                    .map_err(|e| anyhow::anyhow!("Error sending command to watcher: {}", e))?;
            } else {
                error!(
                    address = httpserver_service.address,
                    "Invalid address in manifest"
                );
            }
        }
        Ok(())
    }

    /// Handles a manifest unpublished event.
    fn handle_manifest_unpublished(&self, mu: ManifestUnpublished) -> Result<()> {
        self.tx
            .send(WatcherCommand::RemoveService {
                name: mu.name,
                namespaces: self.namespaces.clone(),
            })
            .map_err(|e| anyhow::anyhow!("Error sending command to watcher: {}", e))?;
        Ok(())
    }
}

/// Waits for commands to update or remove services based on manifest deploy/undeploy events in
/// underlying lattices.
/// Each lattice is managed by a [`Watcher`] which listens for events relayed by a NATS consumer and
/// issues commands to create or update services in the cluster.
pub struct ServiceWatcher {
    watchers: Arc<RwLock<HashMap<String, Watcher>>>,
    sender: mpsc::UnboundedSender<WatcherCommand>,
    stream_replicas: u16,
}

impl ServiceWatcher {
    /// Creates a new service watcher.
    pub fn new(k8s_client: KubeClient, stream_replicas: u16) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<WatcherCommand>();

        let client = k8s_client.clone();
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    WatcherCommand::UpsertService(params) => {
                        create_or_update_service(client.clone(), &params, None)
                            .await
                            .map_err(|e| error!(err=%e, "Error creating/updating service"))
                            .ok();
                    }
                    WatcherCommand::RemoveService { name, namespaces } => {
                        for namespace in namespaces {
                            delete_service(client.clone(), &namespace, name.as_str())
                                .await
                                .map_err(|e| error!(err=%e, %namespace, "Error deleting service"))
                                .ok();
                        }
                    }
                    WatcherCommand::RemoveServices { namespaces } => {
                        for namespace in namespaces {
                            delete_services(client.clone(), namespace.as_str())
                                .await
                                .map_err(|e| error!(err=%e, %namespace, "Error deleting service"))
                                .ok();
                        }
                    }
                }
            }
        });

        Self {
            watchers: Arc::new(RwLock::new(HashMap::new())),
            sender: tx,
            stream_replicas,
        }
    }

    /// Reconciles services for a set of apps in a lattice.
    /// This intended to be called by the controller whenever it reconciles state.
    pub async fn reconcile_services(&self, apps: Vec<ModelSummary>, lattice_id: String) {
        if let Some(watcher) = self.watchers.read().await.get(lattice_id.as_str()) {
            let wadm_client =
                WadmClient::from_nats_client(&lattice_id, None, watcher.nats_client.clone());
            for app in apps {
                if app.deployed_version.is_none() {
                    continue;
                }
                match wadm_client
                    .get_manifest(app.name.as_str(), app.deployed_version.as_deref())
                    .await
                {
                    Ok(manifest) => {
                        let _ = watcher.handle_manifest_published(ManifestPublished {
                                manifest,
                            }).map_err(|e| error!(err = %e, %lattice_id, app = %app.name, "failed to trigger service reconciliation for app"));
                    }
                    Err(e) => warn!(err=%e, "Unable to retrieve model"),
                };
            }
        };
    }

    /// Create a new [`Watcher`] for a lattice.
    /// It will return early if a [`Watcher`] already exists for the lattice.
    pub async fn watch(&self, client: Client, namespace: String, lattice_id: String) -> Result<()> {
        // If we're already watching this lattice then return early
        // TODO is there an easy way to do this with a read lock?
        let mut watchers = self.watchers.write().await;
        if let Some(watcher) = watchers.get_mut(lattice_id.as_str()) {
            watcher.namespaces.insert(namespace);
            return Ok(());
        }

        let js = jetstream::new(client.clone());

        // Should we also be doing this when we first create the ServiceWatcher?
        let stream = js
            .get_or_create_stream(StreamConfig {
                name: OPERATOR_STREAM_NAME.to_string(),
                description: Some(
                    "Stream for wadm events consumed by the wasmCloud K8s Operator".to_string(),
                ),
                max_age: wadm::DEFAULT_EXPIRY_TIME,
                retention: RetentionPolicy::WorkQueue,
                storage: StorageType::File,
                allow_rollup: false,
                num_replicas: self.stream_replicas as usize,
                mirror: Some(Source {
                    name: WADM_EVENT_STREAM_NAME.to_string(),
                    subject_transforms: vec![SubjectTransform {
                        source: wadm::DEFAULT_WADM_EVENTS_TOPIC.to_string(),
                        destination: OPERATOR_STREAM_SUBJECT.replacen('*', "{{wildcard(1)}}", 1),
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await?;

        let consumer_name = format!("{CONSUMER_PREFIX}-{}", lattice_id.clone());
        let consumer = stream
        .get_or_create_consumer(
            consumer_name.as_str(),
            Config {
                durable_name: Some(consumer_name.clone()),
                description: Some("Consumer created by the wasmCloud K8s Operator to watch for new service endpoints in wadm manifests".to_string()),
                ack_policy: jetstream::consumer::AckPolicy::Explicit,
                ack_wait: std::time::Duration::from_secs(2),
                max_deliver: 3,
                deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::All,
                filter_subject: OPERATOR_STREAM_SUBJECT.replacen('*', &lattice_id, 1),
                ..Default::default()
            },
        )
        .await?;

        let watcher = Watcher::new(
            namespace,
            lattice_id.clone(),
            client.clone(),
            consumer,
            self.sender.clone(),
        );
        watchers.insert(lattice_id.clone(), watcher);
        Ok(())
    }

    /// Stops watching a lattice by stopping the underlying [`Watcher`] if no namespaces require it.
    pub async fn stop_watch(&self, lattice_id: String, namespace: String) -> Result<()> {
        let mut watchers = self.watchers.write().await;
        if let Some(watcher) = watchers.get_mut(lattice_id.as_str()) {
            watcher.namespaces.remove(namespace.as_str());
            if watcher.namespaces.is_empty() {
                watchers.remove(lattice_id.as_str());
            }

            self.sender
                .send(WatcherCommand::RemoveServices {
                    namespaces: HashSet::from([namespace]),
                })
                .map_err(|e| anyhow::anyhow!("Error sending command to watcher: {}", e))?;
        }
        Ok(())
    }
}

/// Creates or updates a service in the cluster based on the provided parameters.
pub async fn create_or_update_service(
    k8s_client: KubeClient,
    params: &ServiceParams,
    owner_ref: Option<OwnerReference>,
) -> Result<()> {
    let mut labels = common_labels();
    labels.extend(BTreeMap::from([(
        "app.kubernetes.io/name".to_string(),
        params.name.to_string(),
    )]));
    let mut selector = BTreeMap::new();
    let mut create_endpoints = false;
    if let Some(host_labels) = &params.host_labels {
        selector.insert(
            "app.kubernetes.io/name".to_string(),
            "wasmcloud".to_string(),
        );
        selector.extend(
            host_labels
                .iter()
                .map(|(k, v)| (format_service_selector(k), v.clone())),
        );
    } else {
        create_endpoints = true;
    }

    for namespace in params.namespaces.iter() {
        let api = Api::<Service>::namespaced(k8s_client.clone(), namespace);

        let mut svc = Service {
            metadata: kube::api::ObjectMeta {
                name: Some(params.name.clone()),
                labels: Some(labels.clone()),
                finalizers: Some(vec![SERVICE_FINALIZER.to_string()]),
                namespace: Some(namespace.clone()),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                selector: Some(selector.clone()),
                ports: Some(vec![ServicePort {
                    name: Some("http".to_string()),
                    port: params.port as i32,
                    protocol: Some("TCP".to_string()),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };

        if let Some(owner_ref) = &owner_ref {
            svc.metadata.owner_references = Some(vec![owner_ref.clone()]);
        }

        debug!(service =? svc, %namespace, "Creating/updating service");

        let svc = api
            .patch(
                params.name.as_str(),
                &PatchParams::apply(SERVICE_FINALIZER),
                &Patch::Apply(svc),
            )
            .await
            .map_err(|e| {
                error!(err = %e, "Error creating/updating service");
                e
            })?;

        if create_endpoints {
            let crds =
                Api::<WasmCloudHostConfig>::namespaced(k8s_client.clone(), namespace.as_str());
            let pods = Api::<Pod>::namespaced(k8s_client.clone(), namespace.as_str());
            let endpoints =
                Api::<EndpointSlice>::namespaced(k8s_client.clone(), namespace.as_str());

            let configs = crds.list(&ListParams::default()).await?;
            let mut ips = vec![];
            for cfg in configs {
                if cfg.spec.lattice == params.lattice_id {
                    let name = cfg.metadata.name.unwrap();
                    let pods = pods
                        .list(&ListParams {
                            label_selector: Some(format!(
                            "app.kubernetes.io/name=wasmcloud,app.kubernetes.io/instance={name}"
                        )),
                            ..Default::default()
                        })
                        .await?;
                    let pod_ips = pods
                        .into_iter()
                        .filter_map(|pod| {
                            pod.status.and_then(|status| {
                                if status.phase == Some("Running".to_string()) {
                                    status.pod_ips
                                } else {
                                    None
                                }
                            })
                        })
                        .flatten();
                    ips.extend(pod_ips);
                }
            }

            // Create an EndpointSlice if we're working with a daemonscaler without label requirements.
            // This means we need to manually map the endpoints to each wasmCloud host belonging to the
            // lattice in this namespace.
            // TODO: This can actually span namespaces, same with the label requirements so should we
            // be querying _all_ CRDs to find all available pods?
            if !ips.is_empty() {
                let mut labels = labels.clone();
                labels.insert(
                    "kubernetes.io/service-name".to_string(),
                    params.name.clone(),
                );
                let endpoint_slice = EndpointSlice {
                    metadata: kube::api::ObjectMeta {
                        name: Some(params.name.clone()),
                        labels: Some(labels.clone()),
                        // SAFETY: This should be safe according to the kube.rs docs, which specifiy
                        // that anything created through the apiserver should have a populated field
                        // here.
                        owner_references: Some(vec![svc.controller_owner_ref(&()).unwrap()]),
                        ..Default::default()
                    },
                    // TODO is there a way to figure this out automatically? Maybe based on the number
                    // of IPs that come back or what they are
                    address_type: "IPv4".to_string(),
                    endpoints: ips
                        .iter()
                        .filter_map(|ip| {
                            ip.ip.as_ref().map(|i| Endpoint {
                                addresses: vec![i.clone()],
                                conditions: Some(EndpointConditions {
                                    ready: Some(true),
                                    serving: Some(true),
                                    terminating: None,
                                }),
                                hostname: None,
                                target_ref: None,
                                ..Default::default()
                            })
                        })
                        .collect(),
                    ports: Some(vec![EndpointPort {
                        name: Some("http".to_string()),
                        port: Some(params.port as i32),
                        protocol: Some("TCP".to_string()),
                        app_protocol: None,
                    }]),
                };
                // TODO this should probably do the usual get/patch or get/replce bit since I don't
                // think this is fully syncing endpoints when pods are deleted. Also we should update
                // this based on pod status since we may end up having stale IPs
                endpoints
                    .patch(
                        params.name.as_str(),
                        &PatchParams::apply(CLUSTER_CONFIG_FINALIZER),
                        &Patch::Apply(endpoint_slice),
                    )
                    .await
                    .map_err(|e| {
                        error!("Error creating endpoint slice: {}", e);
                        e
                    })?;
            }
        };
    }

    debug!("Created/updated service");
    Ok(())
}

#[derive(Default)]
pub struct HttpServerComponent {
    labels: Option<HashMap<String, String>>,
    address: String,
}

/// Finds the httpserver component in a manifest and returns the details needed to create a service
fn http_server_component(manifest: &Manifest) -> Option<HttpServerComponent> {
    let components: Vec<&Component> = manifest
        .components()
        // filter just for the wasmCloud httpserver for now. This should actually just filter for
        // the http capability
        .filter(|c| {
            if let Properties::Capability { properties } = &c.properties {
                if properties
                    .image
                    .starts_with("ghcr.io/wasmcloud/http-server")
                {
                    return true;
                }
            }
            false
        })
        .collect();

    let scalers: Vec<&Trait> = components
        .iter()
        .filter_map(|c| {
            if let Some(t) = &c.traits {
                for trait_ in t {
                    if trait_.trait_type == "daemonscaler" {
                        return Some(trait_);
                    }
                }
            };
            None
        })
        .collect();

    // Right now we only support daemonscalers, so if we don't find any then we have nothing to do
    if scalers.is_empty() {
        return None;
    }

    let links: Vec<&Trait> = components
        .iter()
        .filter_map(|c| {
            if let Some(t) = &c.traits {
                for trait_ in t {
                    if trait_.trait_type == "link" {
                        return Some(trait_);
                    }
                }
            };
            None
        })
        .collect();

    let mut details = HttpServerComponent::default();
    let mut should_create_service = false;
    for l in links {
        match &l.properties {
            TraitProperty::Link(props) => {
                if props.namespace == "wasi"
                    && props.package == "http"
                    && props.interfaces.contains(&"incoming-handler".to_string())
                    && props.source.is_some()
                {
                    let source = props.source.as_ref().unwrap();
                    for cp in source.config.iter() {
                        if let Some(addr) = cp.properties.as_ref().and_then(|p| p.get("address")) {
                            details.address.clone_from(addr);
                            should_create_service = true;
                        }
                    }
                }
            }
            TraitProperty::SpreadScaler(scaler) => {
                for spread in scaler.spread.iter() {
                    spread.requirements.iter().for_each(|(k, v)| {
                        details
                            .labels
                            .get_or_insert_with(HashMap::new)
                            .insert(k.clone(), v.clone());
                    });
                }
            }
            _ => {}
        }
    }

    if should_create_service {
        return Some(details);
    }
    None
}

/// Deletes a service in the cluster.
async fn delete_service(k8s_client: KubeClient, namespace: &str, name: &str) -> Result<()> {
    debug!(namespace = namespace, name = name, "Deleting service");
    let api = Api::<Service>::namespaced(k8s_client.clone(), namespace);
    // Remove the finalizer so that the service can be deleted
    let mut svc = api.get(name).await?;
    svc.metadata.finalizers = None;
    svc.metadata.managed_fields = None;
    api.patch(
        name,
        &PatchParams::apply(SERVICE_FINALIZER).force(),
        &Patch::Apply(svc),
    )
    .await
    .map_err(|e| {
        error!("Error removing finalizer from service: {}", e);
        e
    })?;

    api.delete(name, &DeleteParams::default()).await?;
    Ok(())
}

async fn delete_services(k8s_client: KubeClient, namespace: &str) -> Result<()> {
    let api = Api::<Service>::namespaced(k8s_client.clone(), namespace);
    let services = api
        .list(&ListParams {
            label_selector: Some(WASMCLOUD_OPERATOR_MANAGED_BY_LABEL_REQUIREMENT.to_string()),
            ..Default::default()
        })
        .await?;
    for svc in services {
        let name = svc.metadata.name.unwrap();
        delete_service(k8s_client.clone(), namespace, name.as_str()).await?;
    }
    Ok(())
}

/// Formats a service selector for a given name.
fn format_service_selector(name: &str) -> String {
    format!("{WASMCLOUD_OPERATOR_HOST_LABEL_PREFIX}/{}", name)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_daemonscaler_should_return() {
        let manifest = r#"
apiVersion: core.oam.dev/v1beta1
kind: Application
metadata:
  name: rust-http-hello-world
  annotations:
    version: v0.0.1
    description: "HTTP hello world demo in Rust, using the WebAssembly Component Model and WebAssembly Interfaces Types (WIT)"
    experimental: "true"
spec:
  components:
    - name: http-hello-world
      type: component
      properties:
        image: wasmcloud.azurecr.io/http-hello-world:0.1.0
        id: helloworld
      traits:
        # Govern the spread/scheduling of the actor
        - type: spreadscaler
          properties:
            replicas: 5000
    # Add a capability provider that mediates HTTP access
    - name: httpserver
      type: capability
      properties:
        image: ghcr.io/wasmcloud/http-server:0.20.0
        id: httpserver
      traits:
        # Link the HTTP server, and inform it to listen on port 8080
        # on the local machine
        - type: link
          properties:
            target: http-hello-world
            namespace: wasi
            package: http
            interfaces: [incoming-handler]
            source_config:
            - name: default-http
              properties:
                address: 0.0.0.0:8080
        - type: daemonscaler
          properties:
            replicas: 1
"#;
        let m = serde_yaml::from_str::<Manifest>(manifest).unwrap();
        let component = http_server_component(&m);
        assert!(component.is_some());

        let manifest = r#"
apiVersion: core.oam.dev/v1beta1
kind: Application
metadata:
  name: rust-http-hello-world
  annotations:
    version: v0.0.1
    description: "HTTP hello world demo in Rust, using the WebAssembly Component Model and WebAssembly Interfaces Types (WIT)"
    experimental: "true"
spec:
  components:
    - name: http-hello-world
      type: component
      properties:
        image: wasmcloud.azurecr.io/http-hello-world:0.1.0
        id: helloworld
      traits:
        # Govern the spread/scheduling of the actor
        - type: spreadscaler
          properties:
            replicas: 5000
    # Add a capability provider that mediates HTTP access
    - name: httpserver
      type: capability
      properties:
        image: ghcr.io/wasmcloud/http-server:0.20.0
        id: httpserver
      traits:
        # Link the HTTP server, and inform it to listen on port 8080
        # on the local machine
        - type: link
          properties:
            target: http-hello-world
            namespace: wasi
            package: http
            interfaces: [incoming-handler]
            source_config:
            - name: default-http
              properties:
                address: 0.0.0.0:8080
"#;
        let m = serde_yaml::from_str::<Manifest>(manifest).unwrap();
        let component = http_server_component(&m);
        assert!(component.is_none());
    }
}
