use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Generate the Kubernetes wrapper struct `Garage` from our Spec and Status struct
///
/// This provides a hook for generating the CRD yaml (in crdgen.rs)
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "Garage",
    group = "deuxfleurs.fr",
    version = "v0alpha",
    status = "GarageStatus",
    namespaced,
    printcolumn = r#"{ "name": "region", "type": "string", "description": "configured region", "jsonPath": ".spec.config.region" }"#,
    printcolumn = r#"{ "name": "replication", "type": "string", "description": "configured replication mode", "jsonPath": ".spec.config.replicationMode" }"#,
    printcolumn = r#"{ "name": "status", "type": "string", "description": "garage status", "jsonPath": ".status" }"#
)]
pub struct GarageSpec {
    pub autolayout: bool,
    pub config: Option<GarageConfig>,
    pub secrets: Option<GarageSecrets>,
    pub storage: Option<GarageStorage>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GarageConfig {
    pub ports: PortConfig,
    pub region: String,
    pub replication_mode: String,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GarageSecrets {
    pub admin: Option<SecretReference>,
    pub rpc: Option<SecretReference>,
}

/// The status object of `Garage`
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
pub enum GarageStatus {
    #[default]
    Creating,
    LayingOut,
    Ready,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GarageStorage {
    pub meta: VolumeConfig,
    pub data: VolumeConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PortConfig {
    pub admin: u16,
    pub rpc: u16,
    pub s3_api: u16,
    pub s3_web: u16,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VolumeConfig {
    pub existing_claim: Option<String>,

    pub size: Option<String>,
    pub storage_class: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecretReference {
    pub secret_name: Option<String>,
    pub namespace: Option<String>,
}

impl Default for PortConfig {
    fn default() -> Self {
        Self {
            admin: 3903,
            rpc: 3901,
            s3_api: 3900,
            s3_web: 3902,
        }
    }
}

impl Default for GarageConfig {
    fn default() -> Self {
        Self {
            ports: PortConfig::default(),
            region: "garage".into(),
            replication_mode: "none".into(),
        }
    }
}
