use k8s_openapi::api::core::v1::SecretReference;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Specification for a Garage server instance
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    kind = "Garage",
    group = "deuxfleurs.fr",
    version = "v0alpha",
    status = "GarageStatus",
    doc = "A Garage server instance",
    namespaced,
    printcolumn = r#"{ "name": "region", "type": "string", "description": "configured region", "jsonPath": ".spec.config.region" }"#,
    printcolumn = r#"{ "name": "replication", "type": "string", "description": "configured replication mode", "jsonPath": ".spec.config.replicationMode" }"#,
    printcolumn = r#"{ "name": "capacity", "type": "integer", "description": "garage capacity", "jsonPath": ".status.capacity" }"#,
    printcolumn = r#"{ "name": "status", "type": "string", "description": "garage status", "jsonPath": ".status.state" }"#
)]
#[serde(rename_all = "camelCase")]
pub struct GarageSpec {
    /// Whether or not to auto-layout the garage instance
    ///
    /// Garage has a notion of layouts in order to allow instances to cluster
    /// up after the fact. While useful, this generally makes it more difficult to
    /// set up without manual intervention.
    ///
    /// If auto_layout is enabled, the operator will use the configuration supplied
    /// in config to automatically layout the garage instance for you.
    #[serde(default)]
    pub auto_layout: bool,

    /// The config for this garage instance.
    ///
    /// Most of these options are mirrored from the
    /// [official docs](https://garagehq.deuxfleurs.fr/documentation/reference-manual/configuration/).
    #[serde(default)]
    pub config: GarageConfig,

    /// Configuration for where to store the secrets needed for interacting with garage.
    #[serde(default)]
    pub secrets: GarageSecrets,

    /// The storage backing for this garage instance.
    pub storage: GarageStorage,
}

/// Configuration for a garage instance.
///
/// Refer to the [official docs](https://garagehq.deuxfleurs.fr/documentation/reference-manual/configuration/).
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GarageConfig {
    /// Listening port configuration
    #[serde(default)]
    pub ports: PortConfig,

    /// The [S3 region](https://garagehq.deuxfleurs.fr/documentation/reference-manual/configuration/#s3_region) for this instance.
    ///
    /// Must be the same when linking up separate instances.
    #[serde(default = "defaults::region")]
    pub region: String,

    /// The type of [replication mode](https://garagehq.deuxfleurs.fr/documentation/reference-manual/configuration/#replication_mode).
    #[serde(default = "defaults::replication")]
    pub replication_mode: String,
}

/// Secrets configuration for a Garage instance.
#[derive(Debug, Default, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct GarageSecrets {
    /// Reference to the [admin API](https://garagehq.deuxfleurs.fr/documentation/reference-manual/admin-api/) secret.
    pub admin: Option<SecretReference>,

    /// Reference to the inter-garage RPC secret.
    pub rpc: Option<SecretReference>,
}

/// Configuration for the backing store of a Garage instance.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GarageStorage {
    /// Backing to use for storing block metadata.
    pub meta: String,

    /// List of backings to use for storing data.
    pub data: Vec<String>,
}

/// Port configuration of a Garage instance.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct PortConfig {
    /// Port used for the [admin API](https://garagehq.deuxfleurs.fr/documentation/reference-manual/admin-api/)
    pub admin: u16,

    /// Port used for the inter-garage RPC.
    pub rpc: u16,

    /// Port used for handling S3 API traffic.
    pub s3_api: u16,

    /// Port used for hosting buckets as web pages.
    pub s3_web: u16,
}

/// The status of the garage instance
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
pub struct GarageStatus {
    /// The total capacity of this instance
    pub capacity: i64,

    /// The current state of the garage instance
    pub state: GarageState,
}

/// The possible states of a `Garage`
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
pub enum GarageState {
    /// The garage instance is being created.
    #[default]
    Creating,

    /// The garage instance is undergoing layout changes
    LayingOut,

    /// The garage instance is ready to receive traffic.
    Ready,

    /// The garage instance encountered an error.
    Errored,
}

impl Default for GarageConfig {
    fn default() -> Self {
        Self {
            ports: Default::default(),
            region: defaults::region(),
            replication_mode: defaults::replication(),
        }
    }
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

mod defaults {
    pub fn region() -> String {
        "garage".into()
    }
    pub fn replication() -> String {
        "none".into()
    }
}
