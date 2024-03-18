use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::NamespacedReference;

/// A bucket in a garage instance.
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    kind = "Bucket",
    group = "deuxfleurs.fr",
    version = "v0alpha",
    status = "BucketStatus",
    namespaced,
    printcolumn = r#"{ "name": "garage", "type": "string", "description": "owning garage instance", "jsonPath": ".spec.garageRef" }"#,
    printcolumn = r#"{ "name": "quotas", "type": "string", "description": "quotas for this bucket", "jsonPath": ".spec.quotas" }"#,
    printcolumn = r#"{ "name": "status", "type": "string", "description": "bucket status", "jsonPath": ".status.state" }"#
)]
#[serde(rename_all = "camelCase")]
pub struct BucketSpec {
    /// A reference to the garage instance for this bucket.
    pub garage_ref: NamespacedReference,

    /// Quotas for this bucket.
    #[serde(default)]
    pub quotas: BucketQuotas,
}

/// Quotas for a bucket.
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct BucketQuotas {
    /// The max size any single file.
    pub max_size: Option<Quantity>,

    /// The maximum amount of objects allowed.
    pub max_object_count: Option<usize>,
}

/// The possible states of a bucket
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
pub enum BucketState {
    /// The bucket is in the process of being created.
    #[default]
    Creating,

    /// Configuration changes are being applied.
    Configuring,

    /// The bucket is ready to operate.
    Ready,

    /// The bucket instance encountered an error.
    Errored,
}

/// The status of a bucket
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
pub struct BucketStatus {
    /// The garage internal ID for this bucket
    pub id: String,

    /// The state of the bucket
    pub state: BucketState,
}
