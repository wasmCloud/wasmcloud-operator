use crate::docker_secret::DockerConfigJson;
use crate::{Error, Result};
use anyhow::bail;
use futures::StreamExt;
use handlebars::Handlebars;
use k8s_openapi::api::apps::v1::{DaemonSet, DaemonSetSpec, Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    ConfigMap, ConfigMapVolumeSource, Container, ContainerPort, EnvVar, EnvVarSource, ExecAction,
    Lifecycle, LifecycleHandler, PodSpec, PodTemplateSpec, Secret, SecretKeySelector,
    SecretVolumeSource, Service, ServiceAccount, ServicePort, ServiceSpec, Volume, VolumeMount,
};
use k8s_openapi::api::rbac::v1::{PolicyRule, Role, RoleBinding, RoleRef, Subject};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::{
    api::{Api, ObjectMeta, Patch, PatchParams},
    client::Client,
    runtime::{
        controller::{Action, Config, Controller},
        finalizer::{finalizer, Event as Finalizer},
        watcher,
    },
    Resource, ResourceExt,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::str::from_utf8;
use std::sync::Arc;
use tokio::{sync::RwLock, time::Duration};
use tracing::{info, warn};
use wasmcloud_operator_types::v1alpha1::{
    AppStatus, WasmCloudHostConfig, WasmCloudHostConfigStatus,
};

pub static CLUSTER_CONFIG_FINALIZER: &str = "wasmcloudhost.k8s.wasmcloud.dev";

/// Default host that identifies a host that is running in kubernetes
pub const K8S_HOST_TAG: &str = "kubernetes";
/// wasmCloud custom label for controlling routing
pub const WASMCLOUD_ROUTE_TO_LABEL: &str = "wasmcloud.dev/route-to";

#[derive(Clone)]
pub struct Context {
    pub client: Client,
    pub wasmcloud_config: WasmcloudConfig,
    pub nats_creds: Arc<RwLock<HashMap<NameNamespace, SecretString>>>,
}

#[derive(Clone, Default)]
pub struct Secrets {
    pub wasmcloud_cluster_seed: String,
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
        let wasmcloud_cluster_seed = data.get("WASMCLOUD_CLUSTER_SEED");
        let nats_creds = data.get("nats.creds");

        if wasmcloud_cluster_seed.is_none() {
            bail!(
                "Secret {} has no WASMCLOUD_CLUSTER_SEED",
                secret.metadata.name.as_ref().unwrap()
            );
        };

        Ok(Self {
            wasmcloud_cluster_seed: from_utf8(&wasmcloud_cluster_seed.unwrap().0)?.to_string(),
            nats_creds: match &nats_creds {
                Some(c) => from_utf8(&c.0).ok().map(|s| s.to_string()),
                None => None,
            },
        })
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct WasmcloudConfig {}

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
    let client = ctx.client.clone();
    let ns = config.namespace().unwrap();
    let name = config.name_any();
    let config: Api<WasmCloudHostConfig> = Api::namespaced(client.clone(), &ns);
    let mut cfg = config.get(&name).await?;
    let secrets = Api::<Secret>::namespaced(client, &ns);

    let secret = secrets.get(&cfg.spec.secret_name).await.map_err(|e| {
        warn!("Failed to read secrets: {}", e);
        e
    })?;
    let s = Secrets::from_k8s_secret(&secret).map_err(|e| {
        warn!("Failed to read secrets: {}", e);
        Error::SecretError(format!(
            "Failed to read all secrets from {}: {}",
            secret.metadata.name.unwrap(),
            e
        ))
    })?;

    if let Err(e) = configmap(&cfg, ctx.clone(), s.nats_creds.is_some()).await {
        warn!("Failed to reconcile configmap: {}", e);
        return Err(e);
    };

    if let Some(nats_creds) = &s.nats_creds {
        if let Err(e) = store_nats_creds(&cfg, ctx.clone(), nats_creds.clone()).await {
            warn!("Failed to reconcile secret: {}", e);
            return Err(e);
        };
    }

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

    let nc = s.nats_creds.map(SecretString::new);
    let apps = crate::resources::application::list_apps(
        &cfg.spec.nats_address,
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
    info!("Found apps: {:?}", app_names);
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

    Ok(Action::requeue(Duration::from_secs(5 * 60)))
}

async fn cleanup(config: &WasmCloudHostConfig, ctx: Arc<Context>) -> Result<Action> {
    let client = ctx.client.clone();
    let ns = config.namespace().unwrap();
    let name = config.name_any();
    let _configs: Api<WasmCloudHostConfig> = Api::namespaced(client, &ns);

    let nst = NameNamespace::new(name.clone(), ns.clone());
    ctx.nats_creds.write().await.remove(&nst);

    Ok(Action::await_change())
}

fn pod_template(config: &WasmCloudHostConfig, _ctx: Arc<Context>) -> PodTemplateSpec {
    let mut labels = BTreeMap::new();
    labels.insert("app.kubernetes.io/name".to_string(), config.name_any());
    labels.insert("app.kubernetes.io/instance".to_string(), config.name_any());

    let mut wasmcloud_env = vec![
        EnvVar {
            name: "WASMCLOUD_CLUSTER_SEED".to_string(),
            value_from: Some(EnvVarSource {
                secret_key_ref: Some(SecretKeySelector {
                    name: Some(config.spec.secret_name.clone()),
                    key: "WASMCLOUD_CLUSTER_SEED".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
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
            name: "WASMCLOUD_CLUSTER_ISSUERS".to_string(),
            value: Some(config.spec.issuers.join(",")),
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

    if let Some(labels) = &config.spec.host_labels {
        for (k, v) in labels.iter() {
            wasmcloud_env.push(EnvVar {
                name: format!("WASMCLOUD_LABEL_{}", k),
                value: Some(v.clone()),
                ..Default::default()
            });
        }
    }

    let mut nats_resources: Option<k8s_openapi::api::core::v1::ResourceRequirements> = None;
    let mut wasmcloud_resources: Option<k8s_openapi::api::core::v1::ResourceRequirements> = None;
    if let Some(scheduling_options) = &config.spec.scheduling_options {
        if let Some(resources) = &scheduling_options.resources {
            nats_resources = resources.nats.clone();
            wasmcloud_resources = resources.wasmcloud.clone();
        }
    }

    let containers = vec![
        Container {
            name: "nats-leaf".to_string(),
            image: Some("nats:2.10-alpine".to_string()),
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
            volume_mounts: Some(vec![
                VolumeMount {
                    name: "nats-config".to_string(),
                    mount_path: "/nats/nats.conf".to_string(),
                    read_only: Some(true),
                    sub_path: Some("nats.conf".to_string()),
                    ..Default::default()
                },
                VolumeMount {
                    name: "nats-creds".to_string(),
                    mount_path: "/nats/nats.creds".to_string(),
                    sub_path: Some("nats.creds".to_string()),
                    read_only: Some(true),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        },
        Container {
            name: "wasmcloud-host".to_string(),
            image: Some(format!("wasmcloud/wasmcloud:{}", config.spec.version)),
            env: Some(wasmcloud_env),
            resources: wasmcloud_resources,
            ..Default::default()
        },
    ];

    let mut volumes = vec![
        Volume {
            name: "nats-config".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: Some(config.name_any()),
                ..Default::default()
            }),
            ..Default::default()
        },
        Volume {
            name: "nats-creds".to_string(),
            secret: Some(SecretVolumeSource {
                secret_name: Some(config.spec.secret_name.clone()),
                ..Default::default()
            }),
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
            volumes: Some(vec![
                Volume {
                    name: "nats-config".to_string(),
                    config_map: Some(ConfigMapVolumeSource {
                        name: Some(config.name_any()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Volume {
                    name: "nats-creds".to_string(),
                    secret: Some(SecretVolumeSource {
                        secret_name: Some(config.spec.secret_name.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ]),
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
    template
}

fn deployment_spec(config: &WasmCloudHostConfig, ctx: Arc<Context>) -> DeploymentSpec {
    let pod_template = pod_template(config, ctx);
    let mut labels = BTreeMap::new();
    labels.insert("app.kubernetes.io/name".to_string(), config.name_any());
    labels.insert("app.kubernetes.io/instance".to_string(), config.name_any());
    let ls = LabelSelector {
        match_labels: Some(labels.clone()),
        ..Default::default()
    };

    DeploymentSpec {
        replicas: Some(config.spec.host_replicas as i32),
        selector: ls,
        template: pod_template,
        ..Default::default()
    }
}

fn daemonset_spec(config: &WasmCloudHostConfig, ctx: Arc<Context>) -> DaemonSetSpec {
    let pod_template = pod_template(config, ctx);
    let mut labels = BTreeMap::new();
    labels.insert("app.kubernetes.io/name".to_string(), config.name_any());
    labels.insert("app.kubernetes.io/instance".to_string(), config.name_any());
    let ls = LabelSelector {
        match_labels: Some(labels.clone()),
        ..Default::default()
    };

    DaemonSetSpec {
        selector: ls,
        template: pod_template,
        ..Default::default()
    }
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
                name: "OCI_REGISTRY_USER".to_string(),
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
                name: "OCI_REGISTRY_PASSWORD".to_string(),
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
                name: "OCI_REGISTRY".to_string(),
                value: Some(docker_config.auths.keys().next().unwrap().clone()),
                ..Default::default()
            },
        ];
    }

    if let Some(scheduling_options) = &config.spec.scheduling_options {
        if scheduling_options.daemonset {
            let mut spec = daemonset_spec(config, ctx.clone());
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
        let mut spec = deployment_spec(config, ctx.clone());
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
    label_selector.insert("app.kubernetes.io/name".to_string(), config.name_any());
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
      url: "{{cluster_url}}:7422"
      {{#if use_credentials}}
      credentials: "/nats/nats.creds"
      {{/if}}
    }
  ]
}
"#;
    let tpl = Handlebars::new();
    let rendered = tpl.render_template(template, &json!({"jetstream_domain": config.spec.leaf_node_domain, "cluster_url": config.spec.nats_address, "use_credentials": use_nats_creds}))?;
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
    pub config: WasmcloudConfig,
}

impl State {
    pub fn new(config: WasmcloudConfig) -> Self {
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
    let client = Client::try_default().await?;

    let configs = Api::<WasmCloudHostConfig>::all(client.clone());
    let cms = Api::<ConfigMap>::all(client.clone());
    let deployments = Api::<Deployment>::all(client.clone());
    let secrets = Api::<Secret>::all(client.clone());

    let config = Config::default();
    let ctx = Context {
        client,
        wasmcloud_config: state.config.clone(),
        nats_creds: state.nats_creds.clone(),
    };

    Controller::new(configs, watcher::Config::default())
        .owns(cms, watcher::Config::default())
        .owns(deployments, watcher::Config::default())
        .owns(secrets, watcher::Config::default())
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
