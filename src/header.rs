use axum::headers::{Error, Header, HeaderName, HeaderValue};
use axum::http::header::ACCEPT;
use std::fmt;

#[derive(Debug)]
pub enum As {
    APIGroupDiscoveryList,
    PartialObjectMetadataList,
    Table,
    NotSpecified,
}

impl fmt::Display for As {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            As::APIGroupDiscoveryList => write!(f, "APIGroupDiscoveryList"),
            As::PartialObjectMetadataList => write!(f, "PartialObjectMetadataList"),
            As::Table => write!(f, "Table"),
            As::NotSpecified => write!(f, "NotSpecified"),
        }
    }
}

impl From<Accept> for As {
    fn from(accept: Accept) -> Self {
        accept.0
    }
}

#[derive(Debug)]
pub struct Accept(As);

impl std::fmt::Display for Accept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        match self.0 {
            As::APIGroupDiscoveryList => write!(f, "APIGroupDiscoveryList"),
            As::PartialObjectMetadataList => write!(f, "PartialObjectMetadataList"),
            As::Table => write!(f, "Table"),
            As::NotSpecified => write!(f, "NotSpecified"),
        }
    }
}

// Parses Accept headers from kube-apiserver/aggregator and turns them into Enum values.
// Some examples include:
// * application/json;g=apidiscovery.k8s.io;v=v2beta1;as=APIGroupDiscoveryList
// * application/json;as=Table;g=meta.k8s.io;v=v1
impl Header for Accept {
    fn name() -> &'static HeaderName {
        &ACCEPT
    }
    fn decode<'i, I>(values: &mut I) -> Result<Self, Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        let header_value = values.next();
        // We need to return a default rather than error here, because without it
        // kube-aggregator will freak out about the responses, for some reason.
        if header_value.is_none() {
            return Ok(Accept(As::NotSpecified));
        }

        let value = header_value.unwrap().to_str().unwrap_or("n/a");

        let parts: Vec<&str> = value
            .split(';')
            .filter(|p| p.starts_with("as="))
            .map(|p| p.strip_prefix("as=").unwrap_or(""))
            .collect();

        let header = match parts.into_iter().nth(0) {
            Some("APIGroupDiscoveryList") => Accept(As::APIGroupDiscoveryList),
            Some("PartialObjectMetadataList") => Accept(As::PartialObjectMetadataList),
            Some("Table") => Accept(As::Table),
            None => Accept(As::NotSpecified),
            // This exists to satisfy rust-analyzer, we should probably try to
            // figure out if a new type was added that we need to be responding to.
            Some(&_) => Accept(As::NotSpecified),
        };
        Ok(header)
    }
    fn encode<E>(&self, values: &mut E)
    where
        E: Extend<HeaderValue>,
    {
        let s = self.0.to_string();
        let value = HeaderValue::from_static(s.leak());
        values.extend(std::iter::once(value));
    }
}
