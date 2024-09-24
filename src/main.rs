use anyhow::{anyhow, Result};
use axum_server::{tls_rustls::RustlsConfig, Handle};
use controller::{config::OperatorConfig, State};

use config::Config;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::kube_aggregator::pkg::apis::apiregistration::v1::{
    APIService, APIServiceSpec, ServiceReference,
};
use kube::{
    api::{Api, Patch, PatchParams, PostParams},
    client::Client,
    CustomResourceExt,
};
use opentelemetry::KeyValue;
use opentelemetry_sdk::{
    trace::{RandomIdGenerator, Sampler},
    Resource,
};
use std::io::IsTerminal;
use std::net::SocketAddr;
use std::time::Duration;
use tracing::{error, info};
use tracing_subscriber::layer::SubscriberExt;

use wasmcloud_operator_types::v1alpha1::WasmCloudHostConfig;

#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "-V" || arg == "--version") {
        let version = version();
        println!("{} {version}", env!("CARGO_BIN_NAME"));
        std::process::exit(0);
    }

    let tracing_enabled = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok();
    configure_tracing(tracing_enabled).map_err(|e| {
        error!("Failed to configure tracing: {}", e);
        e
    })?;
    info!("Starting operator");

    let cfg = Config::builder()
        .add_source(config::Environment::with_prefix("WASMCLOUD_OPERATOR"))
        .build()
        .map_err(|e| anyhow!("Failed to build config: {}", e))?;
    let config: OperatorConfig = cfg
        .try_deserialize()
        .map_err(|e| anyhow!("Failed to parse config: {}", e))?;

    let client = Client::try_default().await?;
    install_crd(&client).await?;

    let state = State::new(config);
    let ctl = controller::run(state.clone());
    let router = controller::router::setup(state.clone());

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let tls_config = RustlsConfig::from_der(
        vec![cert.serialize_der()?],
        cert.serialize_private_key_der(),
    )
    .await?;

    let handle = Handle::new();
    let addr = SocketAddr::from(([0, 0, 0, 0], 8443));
    let server = axum_server::bind_rustls(addr, tls_config)
        .handle(handle.clone())
        .serve(router.into_make_service());
    tokio::spawn(async move {
        info!("Starting apiserver");
        let res = server.await;
        if let Err(e) = res {
            error!("Error running apiserver: {}", e);
        }
    });

    ctl.await?;
    handle.graceful_shutdown(Some(Duration::from_secs(3)));
    info!("Controller finished");
    Ok(())
}

fn configure_tracing(enabled: bool) -> anyhow::Result<()> {
    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(opentelemetry_otlp::new_exporter().tonic())
        .with_trace_config(
            opentelemetry_sdk::trace::config()
                .with_sampler(Sampler::AlwaysOn)
                .with_id_generator(RandomIdGenerator::default())
                .with_max_attributes_per_span(32)
                .with_max_events_per_span(32)
                .with_resource(Resource::new(vec![KeyValue::new(
                    "service.name",
                    "wasmcloud-operator",
                )])),
        )
        .install_simple()?;

    let env_filter_layer = tracing_subscriber::EnvFilter::from_default_env();
    let log_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal());

    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    if enabled {
        let subscriber = tracing_subscriber::Registry::default()
            .with(env_filter_layer)
            .with(log_layer)
            .with(otel_layer);

        let _ = tracing::subscriber::set_global_default(subscriber);
    } else {
        let subscriber = tracing_subscriber::Registry::default()
            .with(env_filter_layer)
            .with(log_layer);

        let _ = tracing::subscriber::set_global_default(subscriber);
    }
    Ok(())
}

// Initialize the CRD in the cluster
async fn install_crd(client: &Client) -> anyhow::Result<()> {
    let crds = Api::<CustomResourceDefinition>::all(client.clone());
    let crd = &WasmCloudHostConfig::crd();
    let registrations = Api::<APIService>::all(client.clone());

    let crd_name = crd.metadata.name.as_ref().unwrap();
    // TODO(protochron) validate that the crd is upd to date and patch if needed
    // This doesn't work for some reason with status subresources if they change, probably because
    // they're not actually embedded in the struct
    if let Ok(old_crd) = crds.get(crd_name.as_str()).await {
        if old_crd != *crd {
            info!("Updating CRD");
            crds.patch(
                crd_name.as_str(),
                // https://github.com/kubernetes/client-go/issues/1036
                // regarding +yaml: https://kubernetes.io/docs/reference/using-api/server-side-apply/#serialization
                &PatchParams::apply("application/apply-patch+yaml").force(),
                &Patch::Apply(crd),
            )
            .await?;
        }
    } else {
        crds.create(&PostParams::default(), crd)
            .await
            .map_err(|e| anyhow!("failed to create crd: {e}"))?;
    }

    let namespace = std::env::var("POD_NAMESPACE").unwrap_or("default".to_string());
    let mut registration = APIService {
        metadata: ObjectMeta {
            name: Some("v1beta1.core.oam.dev".to_string()),
            ..Default::default()
        },
        spec: Some(APIServiceSpec {
            group: Some("core.oam.dev".to_string()),
            group_priority_minimum: 2100,
            insecure_skip_tls_verify: Some(true),
            version_priority: 100,
            version: Some("v1beta1".to_string()),
            service: Some(ServiceReference {
                name: Some("wasmcloud-operator".to_string()),
                namespace: Some(namespace),
                port: Some(8443),
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    let old_reg = registrations.get("v1beta1.core.oam.dev").await;
    if let Ok(old) = old_reg {
        info!("Updating APIService");
        let resource_version = old.metadata.resource_version.unwrap();
        registration.metadata.resource_version = Some(resource_version);

        // Wholesale replace because we're terrible people and don't care at all about existing OAM
        // managment controllers
        registrations
            .replace(
                "v1beta1.core.oam.dev",
                &PostParams::default(),
                &registration,
            )
            .await?;
    } else {
        info!("Creating APIService");
        registrations
            .create(&PostParams::default(), &registration)
            .await
            .map_err(|e| anyhow!("failed to create registration: {e}"))?;
    };

    Ok(())
}

fn version() -> &'static str {
    option_env!("CARGO_VERSION_INFO").unwrap_or(env!("CARGO_PKG_VERSION"))
}
