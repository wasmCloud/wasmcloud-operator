use axum::{
    body,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use handlebars::RenderError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("SerializationError: {0}")]
    SerializationError(#[source] serde_json::Error),

    #[error("Kube Error: {0}")]
    KubeError(#[from] kube::Error),

    #[error("Finalizer Error: {0}")]
    // NB: awkward type because finalizer::Error embeds the reconciler error (which is this)
    // so boxing this error to break cycles
    FinalizerError(#[source] Box<kube::runtime::finalizer::Error<Error>>),

    #[error("IllegalDocument")]
    IllegalDocument,

    #[error("NATS error: {0}")]
    NatsError(String),

    #[error("Request error: {0}")]
    RequestError(String),

    #[error("Error retrieving secrets: {0}")]
    SecretError(String),

    #[error("Error rendering template: {0}")]
    RenderError(#[from] RenderError),
}
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let mut resp = Response::new(body::boxed(self.to_string()));
        *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
        resp
    }
}

pub mod controller;
pub mod discovery;
pub mod docker_secret;
pub mod header;
pub(crate) mod openapi;
pub mod resources;
pub mod router;
pub(crate) mod table;

pub use crate::controller::*;
pub use crate::resources::application::{delete_application, get_application, list_applications};
