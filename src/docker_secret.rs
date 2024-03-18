use anyhow::anyhow;
use k8s_openapi::api::core::v1::Secret;
use secrecy::SecretString;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize, Debug)]
pub struct DockerConfigJson {
    pub auths: HashMap<String, DockerConfigJsonAuth>,
}

#[derive(Deserialize, Debug)]
pub struct DockerConfigJsonAuth {
    pub username: String,
    pub password: SecretString,
    pub auth: String,
}

impl DockerConfigJson {
    pub fn from_secret(secret: Secret) -> Result<Self, anyhow::Error> {
        if let Some(data) = secret.data.clone() {
            let bytes = data
                .get(".dockerconfigjson")
                .ok_or(anyhow!("No .dockerconfigjson in secret"))?;
            let b = bytes.clone();

            match std::str::from_utf8(&b.0) {
                Ok(s) => Ok(serde_json::from_str(s)?),
                Err(e) => Err(anyhow!("Error decoding secret: {}", e)),
            }
        } else {
            Err(anyhow!("No data in secret"))
        }
    }
}
