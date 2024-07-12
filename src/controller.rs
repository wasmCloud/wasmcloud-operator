use crate::{
    config::OperatorConfig, docker_secret::DockerConfigJson,
    resources::application::get_nats_client, services::ServiceWatcher, Error, Result,
};
use anyhow::bail;
use futures::StreamExt;
use handlebars::Handlebars;
use k8s_openapi::api::apps::v1::{DaemonSet, DaemonSetSpec, Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    ConfigMap, ConfigMapVolumeSource, Container, ContainerPort, EnvVar, ExecAction, Lifecycle,
    LifecycleHandler, Pod, PodSpec, PodTemplateSpec, Secret, SecretVolumeSource, Service,
    ServiceAccount, ServicePort, ServiceSpec, Volume, VolumeMount,
};
use k8s_openapi::api::rbac::v1::{PolicyRule, Role, RoleBinding, RoleRef, Subject};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::{
    api::{Api, ObjectMeta, Patch, PatchParams},
    client::Client as KubeClient,
    runtime::{
        controller::{Action, Config, Controller},
        finalizer::{finalizer, Event as Finalizer},
        watcher,
    },
    Resource, ResourceExt,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::str::from_utf8;
use std::sync::Arc;
use tokio::{sync::RwLock, time::Duration};
use tracing::{debug, info, warn};
use wasmcloud_operator_types::v1alpha1::{
    AppStatus, WasmCloudHostConfig, WasmCloudHostConfigSpec, WasmCloudHostConfigStatus,
};

pub static CLUSTER_CONFIG_FINALIZER: &str = "operator.k8s.wasmcloud.dev/wasmcloud-host-config";
pub static SERVICE_FINALIZER: &str = "operator.k8s.wasmcloud.dev/service";

/// Default host that identifies a host that is running in kubernetes
pub const K8S_HOST_TAG: &str = "kubernetes";
/// wasmCloud custom label for controlling routing
pub const WASMCLOUD_ROUTE_TO_LABEL: &str = "wasmcloud.dev/route-to";
/// Prefix used to apply host labels to pods
pub const WASMCLOUD_OPERATOR_HOST_LABEL_PREFIX: &str = "host-label.k8s.wasmcloud.dev";
/// Label used to identify resources managed by the wasmcloud operator
pub const WASMCLOUD_OPERATOR_MANAGED_BY_LABEL_REQUIREMENT: &str =
    "app.kubernetes.io/managed-by=wasmcloud-operator";

pub struct Context {
    pub client: KubeClient,
    pub wasmcloud_config: OperatorConfig,
    pub nats_creds: Arc<RwLock<HashMap<NameNamespace, SecretString>>>,
    service_watcher: ServiceWatcher,
}

#[derive(Clone, Default)]
pub struct Secrets {
    pub nats_creds: Option<String>,
}

impl Secrets {
    fn from_k8s_secret(secret: &Secret) -> Result<Self, anyhow::Error> {
        if secret.data.as_ref().is_none() {
            bail!(
                "Secret {} has no data",
                secret.metadata.name.as_ref().unwrap()
            );
        };
        let data = secret.data.as_ref().unwrap();
        let nats_creds = data.get("nats.creds");

        Ok(Self {
            nats_creds: match &nats_creds {
                Some(c) => from_utf8(&c.0).ok().map(|s| s.to_string()),
                None => None,
            },
        })
    }
}

pub async fn reconcile(cluster: Arc<WasmCloudHostConfig>, ctx: Arc<Context>) -> Result<Action> {
    let cluster_configs: Api<WasmCloudHostConfig> =
        Api::namespaced(ctx.client.clone(), &cluster.namespace().unwrap());

    info!(
        "Reconciling WasmCloudHostConfig \"{}\" in {}",
        cluster.name_any(),
        cluster.namespace().unwrap()
    );
    finalizer(
        &cluster_configs,
        CLUSTER_CONFIG_FINALIZER,
        cluster,
        |event| async {
            match event {
                Finalizer::Apply(config) => reconcile_crd(&config, ctx.clone()).await,
                Finalizer::Cleanup(config) => cleanup(&config, ctx.clone()).await,
            }
        },
    )
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

async fn reconcile_crd(config: &WasmCloudHostConfig, ctx: Arc<Context>) -> Result<Action> {
    let kube_client = ctx.client.clone();
    let ns = config.namespace().unwrap();
    let name = config.name_any();
    let config: Api<WasmCloudHostConfig> = Api::namespaced(kube_client.clone(), &ns);
    let mut cfg = config.get(&name).await?;
    let secrets_api = Api::<Secret>::namespaced(kube_client, &ns);

    let mut secrets = Secrets::default();

    if let Some(secret_name) = &cfg.spec.secret_name {
        let kube_secrets = secrets_api.get(secret_name).await.map_err(|e| {
            warn!("Failed to read secrets: {}", e);
            e
        })?;
        secrets = Secrets::from_k8s_secret(&kube_secrets).map_err(|e| {
            warn!("Failed to read secrets: {}", e);
            Error::SecretError(format!(
                "Failed to read all secrets from {}: {}",
                kube_secrets.metadata.name.unwrap(),
                e
            ))
        })?;

        if let Some(nats_creds) = &secrets.nats_creds {
            if let Err(e) = store_nats_creds(&cfg, ctx.clone(), nats_creds.clone()).await {
                warn!("Failed to reconcile secret: {}", e);
                return Err(e);
            };
        }
    }

    if let Err(e) = configmap(&cfg, ctx.clone(), secrets.nats_creds.is_some()).await {
        warn!("Failed to reconcile configmap: {}", e);
        return Err(e);
    };

    // TODO(protochron) these could be split out into separate functions.
    if let Err(e) = configure_auth(&cfg, ctx.clone()).await {
        warn!("Failed to configure auth: {}", e);
        return Err(e);
    };

    if let Err(e) = configure_hosts(&cfg, ctx.clone()).await {
        warn!("Failed to configure deployment: {}", e);
        return Err(e);
    };

    if let Err(e) = configure_service(&cfg, ctx.clone()).await {
        warn!("Failed to configure service: {}", e);
        return Err(e);
    };

    let nc = secrets.nats_creds.map(SecretString::new);
    let apps = crate::resources::application::list_apps(
        &cfg.spec.nats_address,
        &cfg.spec.nats_client_port,
        nc.as_ref(),
        cfg.spec.lattice.clone(),
    )
    .await
    .map_err(|e| {
        warn!("Failed to list apps: {}", e);
        Error::NatsError(format!("Failed to list apps: {}", e))
    })?;

    let app_names = apps
        .iter()
        .map(|a| AppStatus {
            name: a.name.clone(),
            version: a.version.clone(),
        })
        .collect::<Vec<AppStatus>>();
    debug!(app_names=?app_names, "Found apps");
    cfg.status = Some(WasmCloudHostConfigStatus {
        apps: app_names.clone(),
        app_count: app_names.len() as u32,
    });

    config
        .patch_status(
            &name,
            &PatchParams::default(),
            &Patch::Merge(serde_json::json!({ "status": cfg.status.unwrap() })),
        )
        .await
        .map_err(|e| {
            warn!("Failed to update status: {}", e);
            e
        })?;

    // Start the watcher so that services are automatically created in the cluster.
    let nats_client = get_nats_client(
        &cfg.spec.nats_address,
        &cfg.spec.nats_client_port,
        ctx.nats_creds.clone(),
        NameNamespace::new(name.clone(), ns.clone()),
    )
    .await
    .map_err(|e| {
        warn!("Failed to get nats client: {}", e);
        Error::NatsError(format!("Failed to get nats client: {}", e))
    })?;

    ctx.service_watcher
        .watch(nats_client, ns.clone(), cfg.spec.lattice.clone())
        .await
        .map_err(|e| {
            warn!("Failed to start service watcher: {}", e);
            Error::NatsError(format!("Failed to watch services: {}", e))
        })?;

    // Reconcile the services in the lattice so that any existing apps are refected as services in
    // the cluster.
    ctx.service_watcher
        .reconcile_services(apps, cfg.spec.lattice.clone())
        .await;

    Ok(Action::requeue(Duration::from_secs(5 * 60)))
}

async fn cleanup(config: &WasmCloudHostConfig, ctx: Arc<Context>) -> Result<Action> {
    let client = ctx.client.clone();
    let ns = config.namespace().unwrap();
    let name = config.name_any();
    let _configs: Api<WasmCloudHostConfig> = Api::namespaced(client, &ns);

    if config.metadata.deletion_timestamp.is_some() {
        let nst = NameNamespace::new(name.clone(), ns.clone());
        ctx.nats_creds.write().await.remove(&nst);
    }

    debug!(name, namespace = ns, "Cleaning up service");
    ctx.service_watcher
        .stop_watch(config.spec.lattice.clone(), ns.clone())
        .await
        .map_err(|e| {
            warn!("Failed to stop service watcher: {}", e);
            Error::NatsError(format!("Failed to stop service watcher: {}", e))
        })?;

    Ok(Action::await_change())
}

async fn pod_template(config: &WasmCloudHostConfig, ctx: Arc<Context>) -> Result<PodTemplateSpec> {
    let labels = pod_labels(config);

    let mut wasmcloud_env = vec![
        EnvVar {
            name: "WASMCLOUD_STRUCTURED_LOGGING_ENABLED".to_string(),
            value: Some(
                config
                    .spec
                    .enable_structured_logging
                    .unwrap_or(true)
                    .to_string(),
            ),
            ..Default::default()
        },
        EnvVar {
            name: "WASMCLOUD_LOG_LEVEL".to_string(),
            value: Some(config.spec.log_level.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "WASMCLOUD_JS_DOMAIN".to_string(),
            value: Some(config.spec.jetstream_domain.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "WASMCLOUD_LATTICE".to_string(),
            value: Some(config.spec.lattice.clone()),
            ..Default::default()
        },
        // TODO: remove this after wasmCloud 1.0 is released
        EnvVar {
            name: "WASMCLOUD_LATTICE_PREFIX".to_string(),
            value: Some(config.spec.lattice.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "WASMCLOUD_NATS_HOST".to_string(),
            value: Some("127.0.0.1".to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "WASMCLOUD_NATS_PORT".to_string(),
            value: Some("4222".to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "WASMCLOUD_RPC_TIMEOUT_MS".to_string(),
            value: Some("4000".to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "WASMCLOUD_LABEL_kubernetes".to_string(),
            value: Some("true".to_string()),
            ..Default::default()
        },
    ];

    if let Some(prefix) = &config.spec.control_topic_prefix {
        wasmcloud_env.push(EnvVar {
            name: "WASMCLOUD_CTL_TOPIC_PREFIX".to_string(),
            value: Some(prefix.clone()),
            ..Default::default()
        });
    }

    if config.spec.config_service_enabled {
        wasmcloud_env.push(EnvVar {
            name: "WASMCLOUD_CONFIG_SERVICE".to_string(),
            value: Some("true".to_string()),
            ..Default::default()
        });
    }

    if config.spec.allow_latest {
        wasmcloud_env.push(EnvVar {
            name: "WASMCLOUD_OCI_ALLOW_LATEST".to_string(),
            value: Some("true".to_string()),
            ..Default::default()
        });
    }

    if let Some(values) = &config.spec.allowed_insecure {
        wasmcloud_env.push(EnvVar {
            name: "WASMCLOUD_OCI_ALLOWED_INSECURE".to_string(),
            value: Some(values.join(",")),
            ..Default::default()
        });
    }

    if let Some(policy) = &config.spec.policy_service {
        if let Some(subject) = &policy.topic {
            wasmcloud_env.push(EnvVar {
                name: "WASMCLOUD_POLICY_TOPIC".to_string(),
                value: Some(subject.clone()),
                ..Default::default()
            });
        }

        if let Some(changes) = &policy.changes_topic {
            wasmcloud_env.push(EnvVar {
                name: "WASMCLOUD_POLICY_CHANGES_TOPIC".to_string(),
                value: Some(changes.clone()),
                ..Default::default()
            });
        }

        if let Some(timeout) = &policy.timeout_ms {
            wasmcloud_env.push(EnvVar {
                name: "WASMCLOUD_POLICY_TIMEOUT".to_string(),
                value: Some(timeout.to_string()),
                ..Default::default()
            });
        }
    }

    if let Some(labels) = &config.spec.host_labels {
        for (k, v) in labels.iter() {
            wasmcloud_env.push(EnvVar {
                name: format!("WASMCLOUD_LABEL_{}", k),
                value: Some(v.clone()),
                ..Default::default()
            });
        }
    }

    let mut wasmcloud_args = configure_observability(&config.spec);

    let mut nats_resources: Option<k8s_openapi::api::core::v1::ResourceRequirements> = None;
    let mut wasmcloud_resources: Option<k8s_openapi::api::core::v1::ResourceRequirements> = None;
    if let Some(scheduling_options) = &config.spec.scheduling_options {
        if let Some(resources) = &scheduling_options.resources {
            nats_resources = resources.nats.clone();
            wasmcloud_resources = resources.wasmcloud.clone();
        }
    }

    let image = match &config.spec.image {
        Some(i) => i.clone(),
        None => format!("ghcr.io/wasmcloud/wasmcloud:{}", config.spec.version),
    };

    let leaf_image = match &config.spec.nats_leaf_image {
        Some(img) => img.clone(),
        None => "nats:2.10-alpine".to_string(),
    };

    let mut volumes = vec![Volume {
        name: "nats-config".to_string(),
        config_map: Some(ConfigMapVolumeSource {
            name: Some(config.name_any()),
            ..Default::default()
        }),
        ..Default::default()
    }];

    let mut nats_volume_mounts = vec![VolumeMount {
        name: "nats-config".to_string(),
        mount_path: "/nats/nats.conf".to_string(),
        read_only: Some(true),
        sub_path: Some("nats.conf".to_string()),
        ..Default::default()
    }];

    let mut wasm_volume_mounts: Vec<VolumeMount> = vec![];
    if let Some(authorities) = &config
        .spec
        .certificates
        .clone()
        .and_then(|certificates| certificates.authorities)
    {
        for (i, authority) in authorities.iter().enumerate() {
            // configmaps
            if let Some(configmap) = &authority.config_map_ref {
                let volume_name = format!("cert-authority-{}", i);
                let volume_path = format!("/wasmcloud/certificates/authorities/{}", volume_name);
                match discover_configmap_certificates(
                    config.namespace().unwrap_or_default().as_str(),
                    configmap.name.clone().as_str(),
                    &ctx,
                )
                .await
                {
                    Ok(items) => {
                        for item in items {
                            wasmcloud_args.push("--tls-ca-path".to_string());
                            wasmcloud_args.push(format!("{volume_path}/{item}"));
                        }
                    }
                    Err(e) => {
                        if let Some(is_optional) = &configmap.optional {
                            if !is_optional {
                                return Err(e);
                            }
                        } else {
                            continue;
                        }
                    }
                };
                volumes.push(Volume {
                    name: volume_name.clone(),
                    config_map: Some(ConfigMapVolumeSource {
                        name: Some(configmap.name.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                });

                wasm_volume_mounts.push(VolumeMount {
                    name: volume_name.clone(),
                    read_only: Some(true),
                    mount_path: volume_path,
                    ..Default::default()
                });
            }
        }
    }

    if let Some(secret_name) = &config.spec.secret_name {
        volumes.push(Volume {
            name: "nats-creds".to_string(),
            secret: Some(SecretVolumeSource {
                secret_name: Some(secret_name.clone()),
                ..Default::default()
            }),
            ..Default::default()
        });

        nats_volume_mounts.push(VolumeMount {
            name: "nats-creds".to_string(),
            mount_path: "/nats/nats.creds".to_string(),
            sub_path: Some("nats.creds".to_string()),
            read_only: Some(true),
            ..Default::default()
        });
    }

    let containers = vec![
        Container {
            name: "nats-leaf".to_string(),
            image: Some(leaf_image),
            args: Some(vec![
                "-js".to_string(),
                "--config".to_string(),
                "/nats/nats.conf".to_string(),
            ]),
            image_pull_policy: Some("IfNotPresent".to_string()),
            ports: Some(vec![ContainerPort {
                container_port: 4222,
                ..Default::default()
            }]),
            // This is a hack to ensure that the wasmcloud host relays a host stopped event
            lifecycle: Some(Lifecycle {
                pre_stop: Some(LifecycleHandler {
                    exec: Some(ExecAction {
                        command: Some(vec!["sleep".to_string(), "10".to_string()]),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            resources: nats_resources,
            volume_mounts: Some(nats_volume_mounts),
            ..Default::default()
        },
        Container {
            name: "wasmcloud-host".to_string(),
            image: Some(image),
            command: Some(vec!["wasmcloud".to_string()]),
            args: Some(wasmcloud_args),
            env: Some(wasmcloud_env),
            resources: wasmcloud_resources,
            volume_mounts: Some(wasm_volume_mounts),
            ..Default::default()
        },
    ];

    let service_account = config.name_any();
    let mut template = PodTemplateSpec {
        metadata: Some(ObjectMeta {
            labels: Some(labels),
            ..Default::default()
        }),
        spec: Some(PodSpec {
            service_account: Some(config.name_any()),
            containers: containers.clone(),
            volumes: Some(volumes.clone()),
            ..Default::default()
        }),
    };

    if let Some(scheduling_options) = &config.spec.scheduling_options {
        if let Some(pod_overrides) = &scheduling_options.pod_template_additions {
            let mut overrides = pod_overrides.clone();
            overrides.service_account_name = Some(service_account);
            overrides.containers = containers.clone();
            if let Some(vols) = overrides.volumes {
                volumes.extend(vols);
            }
            overrides.volumes = Some(volumes);
            template.spec = Some(overrides);
        }
    };
    Ok(template)
}

async fn discover_configmap_certificates(
    namespace: &str,
    name: &str,
    ctx: &Context,
) -> Result<Vec<String>> {
    let kube_client = ctx.client.clone();
    let api = Api::<ConfigMap>::namespaced(kube_client, &namespace);
    let mut certs = Vec::new();

    let raw_config_map = api.get(name).await?;

    if let Some(raw_data) = raw_config_map.data {
        for (key, _) in raw_data {
            if key.ends_with(".crt") {
                certs.push(key)
            }
        }
    }

    Ok(certs)
}

fn configure_observability(spec: &WasmCloudHostConfigSpec) -> Vec<String> {
    let mut args = Vec::<String>::new();
    if let Some(observability) = &spec.observability {
        if observability.enable {
            args.push("--enable-observability".to_string());
        }
        if !observability.endpoint.is_empty() {
            args.push("--override-observability-endpoint".to_string());
            args.push(observability.endpoint.clone());
        }
        if let Some(protocol) = &observability.protocol {
            args.push("--observability-protocol".to_string());
            args.push(protocol.to_string());
        }
        if let Some(traces) = &observability.traces {
            if traces.enable.unwrap_or(false) {
                args.push("--enable-traces".to_string())
            }
            if let Some(traces_endpoint) = &traces.endpoint {
                args.push("--override-traces-endpoint".to_string());
                args.push(traces_endpoint.to_owned());
            }
        }
        if let Some(metrics) = &observability.metrics {
            if metrics.enable.unwrap_or(false) {
                args.push("--enable-metrics".to_string())
            }
            if let Some(metrics_endpoint) = &metrics.endpoint {
                args.push("--override-metrics-endpoint".to_string());
                args.push(metrics_endpoint.to_owned());
            }
        }
        if let Some(logs) = &observability.logs {
            if logs.enable.unwrap_or(false) {
                args.push("--enable-logs".to_string())
            }
            if let Some(logs_endpoint) = &logs.endpoint {
                args.push("--override-logs-endpoint".to_string());
                args.push(logs_endpoint.to_owned());
            }
        }
    }
    args
}

async fn deployment_spec(
    config: &WasmCloudHostConfig,
    ctx: Arc<Context>,
) -> Result<DeploymentSpec> {
    let pod_template = pod_template(config, ctx).await?;
    let ls = LabelSelector {
        match_labels: Some(selector_labels(config)),
        ..Default::default()
    };

    Ok(DeploymentSpec {
        replicas: Some(config.spec.host_replicas as i32),
        selector: ls,
        template: pod_template,
        ..Default::default()
    })
}

fn pod_labels(config: &WasmCloudHostConfig) -> BTreeMap<String, String> {
    let mut labels = selector_labels(config);
    labels.extend(common_labels());
    if let Some(host_labels) = &config.spec.host_labels {
        for (k, v) in host_labels.iter() {
            labels.insert(
                format!("{WASMCLOUD_OPERATOR_HOST_LABEL_PREFIX}/{}", k.clone()),
                v.clone(),
            );
        }
    }
    labels
}

fn selector_labels(config: &WasmCloudHostConfig) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert(
        "app.kubernetes.io/name".to_string(),
        "wasmcloud".to_string(),
    );
    labels.insert("app.kubernetes.io/instance".to_string(), config.name_any());
    labels
}

async fn daemonset_spec(config: &WasmCloudHostConfig, ctx: Arc<Context>) -> Result<DaemonSetSpec> {
    let pod_template = pod_template(config, ctx).await?;
    let ls = LabelSelector {
        match_labels: Some(selector_labels(config)),
        ..Default::default()
    };

    Ok(DaemonSetSpec {
        selector: ls,
        template: pod_template,
        ..Default::default()
    })
}

async fn configure_hosts(config: &WasmCloudHostConfig, ctx: Arc<Context>) -> Result<()> {
    let mut env_vars = vec![];
    if let Some(registry_credentials) = &config.spec.registry_credentials_secret {
        let secrets_client =
            Api::<Secret>::namespaced(ctx.client.clone(), &config.namespace().unwrap());
        let secret = secrets_client
            .get(registry_credentials)
            .await
            .map_err(|e| {
                warn!("Failed to get image pull secret: {}", e);
                e
            })?;

        let docker_config = DockerConfigJson::from_secret(secret.clone()).map_err(|e| {
            warn!("Failed to convert image pull secret: {}", e);
            Error::SecretError(format!(
                "Failed to convert image pull secret {}: {}",
                secret.metadata.name.clone().unwrap(),
                e
            ))
        })?;

        if docker_config.auths.len() > 1 {
            warn!("Only one registry is supported");
            return Err(Error::SecretError(format!(
                "Only one registry is supported: the secret named {} contains {}",
                secret.metadata.name.clone().unwrap(),
                docker_config.auths.len()
            )));
        }

        env_vars = vec![
            EnvVar {
                name: "WASMCLOUD_OCI_REGISTRY_USER".to_string(),
                value: Some(
                    docker_config
                        .auths
                        .values()
                        .next()
                        .unwrap()
                        .username
                        .clone(),
                ),
                ..Default::default()
            },
            EnvVar {
                name: "WASMCLOUD_OCI_REGISTRY_PASSWORD".to_string(),
                value: Some(
                    docker_config
                        .auths
                        .values()
                        .next()
                        .unwrap()
                        .password
                        .expose_secret()
                        .clone(),
                ),
                ..Default::default()
            },
            EnvVar {
                name: "WASMCLOUD_OCI_REGISTRY".to_string(),
                value: Some(docker_config.auths.keys().next().unwrap().clone()),
                ..Default::default()
            },
        ];
    }

    if let Some(scheduling_options) = &config.spec.scheduling_options {
        if scheduling_options.daemonset {
            let mut spec = daemonset_spec(config, ctx.clone()).await?;
            spec.template.spec.as_mut().unwrap().containers[1]
                .env
                .as_mut()
                .unwrap()
                .append(&mut env_vars);
            let ds = DaemonSet {
                metadata: ObjectMeta {
                    name: Some(config.name_any()),
                    namespace: Some(config.namespace().unwrap()),
                    owner_references: Some(vec![config.controller_owner_ref(&()).unwrap()]),
                    labels: Some(common_labels()),
                    ..Default::default()
                },
                spec: Some(spec),
                ..Default::default()
            };

            let api =
                Api::<DaemonSet>::namespaced(ctx.client.clone(), &config.namespace().unwrap());
            api.patch(
                &config.name_any(),
                &PatchParams::apply(CLUSTER_CONFIG_FINALIZER),
                &Patch::Apply(ds),
            )
            .await?;
        }
    } else {
        let mut spec = deployment_spec(config, ctx.clone()).await?;
        spec.template.spec.as_mut().unwrap().containers[1]
            .env
            .as_mut()
            .unwrap()
            .append(&mut env_vars);
        let deployment = Deployment {
            metadata: ObjectMeta {
                name: Some(config.name_any()),
                namespace: Some(config.namespace().unwrap()),
                owner_references: Some(vec![config.controller_owner_ref(&()).unwrap()]),
                labels: Some(common_labels()),
                ..Default::default()
            },
            spec: Some(spec),
            ..Default::default()
        };

        let api = Api::<Deployment>::namespaced(ctx.client.clone(), &config.namespace().unwrap());
        api.patch(
            &config.name_any(),
            &PatchParams::apply(CLUSTER_CONFIG_FINALIZER),
            &Patch::Apply(deployment),
        )
        .await?;
    };

    Ok(())
}

async fn configure_service(config: &WasmCloudHostConfig, ctx: Arc<Context>) -> Result<()> {
    let mut label_selector = BTreeMap::new();
    label_selector.insert(
        "app.kubernetes.io/name".to_string(),
        "wasmcloud".to_string(),
    );
    label_selector.insert("app.kubernetes.io/instance".to_string(), config.name_any());

    let mut labels = label_selector.clone();
    labels.insert("wasmcloud.dev/route-to".to_string(), "true".to_string());

    let svc = Service {
        metadata: ObjectMeta {
            name: Some(config.name_any()),
            namespace: Some(config.namespace().unwrap()),
            owner_references: Some(vec![config.controller_owner_ref(&()).unwrap()]),
            labels: Some(labels),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            selector: Some(label_selector),
            ports: Some(vec![ServicePort {
                port: 4222,
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    };

    let api = Api::<Service>::namespaced(ctx.client.clone(), &config.namespace().unwrap());
    api.patch(
        &config.name_any(),
        &PatchParams::apply(CLUSTER_CONFIG_FINALIZER),
        &Patch::Apply(svc),
    )
    .await?;
    Ok(())
}

async fn configure_auth(config: &WasmCloudHostConfig, ctx: Arc<Context>) -> Result<()> {
    let svc_account = ServiceAccount {
        metadata: ObjectMeta {
            name: Some(config.name_any()),
            namespace: Some(config.namespace().unwrap()),
            owner_references: Some(vec![config.controller_owner_ref(&()).unwrap()]),
            ..Default::default()
        },
        ..Default::default()
    };

    let api = Api::<ServiceAccount>::namespaced(ctx.client.clone(), &config.namespace().unwrap());
    api.patch(
        &config.name_any(),
        &PatchParams::apply(CLUSTER_CONFIG_FINALIZER),
        &Patch::Apply(svc_account),
    )
    .await?;

    let rules = vec![PolicyRule {
        api_groups: Some(vec!["".to_string()]),
        resources: Some(vec!["services".to_string()]),
        verbs: vec![
            "get".to_string(),
            "list".to_string(),
            "create".to_string(),
            "update".to_string(),
            "patch".to_string(),
            "delete".to_string(),
        ],
        ..Default::default()
    }];
    let role = Role {
        metadata: ObjectMeta {
            name: Some(config.name_any()),
            namespace: Some(config.namespace().unwrap()),
            owner_references: Some(vec![config.controller_owner_ref(&()).unwrap()]),
            ..Default::default()
        },
        rules: Some(rules),
    };
    let api = Api::<Role>::namespaced(ctx.client.clone(), &config.namespace().unwrap());
    api.patch(
        &config.name_any(),
        &PatchParams::apply(CLUSTER_CONFIG_FINALIZER),
        &Patch::Apply(role),
    )
    .await?;

    let role_binding = RoleBinding {
        metadata: ObjectMeta {
            name: Some(config.name_any()),
            namespace: Some(config.namespace().unwrap()),
            owner_references: Some(vec![config.controller_owner_ref(&()).unwrap()]),
            ..Default::default()
        },
        role_ref: RoleRef {
            api_group: "rbac.authorization.k8s.io".to_string(),
            kind: "Role".to_string(),
            name: config.name_any(),
        },
        subjects: Some(vec![Subject {
            kind: "ServiceAccount".to_string(),
            name: config.name_any(),
            namespace: Some(config.namespace().unwrap()),
            ..Default::default()
        }]),
    };
    let api = Api::<RoleBinding>::namespaced(ctx.client.clone(), &config.namespace().unwrap());
    api.patch(
        &config.name_any(),
        &PatchParams::apply(CLUSTER_CONFIG_FINALIZER),
        &Patch::Apply(role_binding),
    )
    .await?;

    Ok(())
}

async fn configmap(
    config: &WasmCloudHostConfig,
    ctx: Arc<Context>,
    use_nats_creds: bool,
) -> Result<()> {
    let template = r#"
jetstream {
  domain: {{jetstream_domain}}
}
leafnodes {
  remotes: [
    {
      url: "{{cluster_url}}:{{leafnode_port}}"
      {{#if use_credentials}}
      credentials: "/nats/nats.creds"
      {{/if}}
    }
  ]
}
"#;
    let tpl = Handlebars::new();
    let rendered = tpl.render_template(template, &json!({"jetstream_domain": config.spec.leaf_node_domain, "cluster_url": config.spec.nats_address, "leafnode_port": config.spec.nats_leafnode_port,"use_credentials": use_nats_creds}))?;
    let mut contents = BTreeMap::new();
    contents.insert("nats.conf".to_string(), rendered);
    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(config.name_any()),
            namespace: Some(config.namespace().unwrap()),
            owner_references: Some(vec![config.controller_owner_ref(&()).unwrap()]),
            ..Default::default()
        },
        data: Some(contents),
        ..Default::default()
    };

    let api = Api::<ConfigMap>::namespaced(ctx.client.clone(), &config.namespace().unwrap());
    api.patch(
        &config.name_any(),
        &PatchParams::apply(CLUSTER_CONFIG_FINALIZER),
        &Patch::Apply(cm),
    )
    .await?;

    Ok(())
}

async fn store_nats_creds(
    config: &WasmCloudHostConfig,
    ctx: Arc<Context>,
    creds: String,
) -> Result<()> {
    let nst = NameNamespace::new(config.name_any(), config.namespace().unwrap());
    ctx.nats_creds
        .write()
        .await
        .insert(nst, SecretString::new(creds));
    Ok(())
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Debug, Hash)]
pub struct NameNamespace {
    pub name: String,
    pub namespace: String,
}

impl NameNamespace {
    pub(crate) fn new(name: String, namespace: String) -> Self {
        Self { name, namespace }
    }
}

#[derive(Clone, Default)]
pub struct State {
    pub nats_creds: Arc<RwLock<HashMap<NameNamespace, SecretString>>>,
    pub config: OperatorConfig,
}

impl State {
    pub fn new(config: OperatorConfig) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }
}

fn error_policy(_object: Arc<WasmCloudHostConfig>, _error: &Error, _ctx: Arc<Context>) -> Action {
    Action::requeue(Duration::from_secs(1))
}

pub async fn run(state: State) -> anyhow::Result<()> {
    let kube_client = KubeClient::try_default().await?;

    let configs = Api::<WasmCloudHostConfig>::all(kube_client.clone());
    let cms = Api::<ConfigMap>::all(kube_client.clone());
    let deployments = Api::<Deployment>::all(kube_client.clone());
    let secrets = Api::<Secret>::all(kube_client.clone());
    let services = Api::<Service>::all(kube_client.clone());
    let pods = Api::<Pod>::all(kube_client.clone());

    let watcher = ServiceWatcher::new(kube_client.clone(), state.config.stream_replicas);
    let config = Config::default();
    let ctx = Context {
        client: kube_client,
        wasmcloud_config: state.config.clone(),
        nats_creds: state.nats_creds.clone(),
        service_watcher: watcher,
    };

    let label_config_watcher = watcher::Config {
        label_selector: Some(common_labels_selector()),
        ..Default::default()
    };

    // TODO: Restrict these to use the right label selectors
    Controller::new(configs, watcher::Config::default())
        .owns(cms, watcher::Config::default())
        .owns(deployments, label_config_watcher.clone())
        .owns(secrets, watcher::Config::default())
        .owns(services, label_config_watcher.clone())
        .owns(pods, label_config_watcher.clone())
        .with_config(config)
        .shutdown_on_signal()
        .run(reconcile, error_policy, Arc::new(ctx))
        .for_each(|res| async move {
            match res {
                Ok(o) => info!("reconciled {:?}", o),
                Err(e) => warn!("reconcile failed: {}", e),
            }
        })
        .await;
    Ok(())
}

pub(crate) fn common_labels() -> BTreeMap<String, String> {
    BTreeMap::from_iter(vec![
        (
            "app.kubernetes.io/name".to_string(),
            "wasmcloud".to_string(),
        ),
        (
            "app.kubernetes.io/managed-by".to_string(),
            "wasmcloud-operator".to_string(),
        ),
    ])
}
fn common_labels_selector() -> String {
    WASMCLOUD_OPERATOR_MANAGED_BY_LABEL_REQUIREMENT.to_string()
}
