use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A bucket in a garage instance
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "Bucket",
    group = "deuxfleurs.fr",
    version = "v0alpha",
    status = "BucketStatus",
    namespaced,
    printcolumn = r#"{ "name": "garage", "type": "string", "description": "owning garage instance", "jsonPath": ".spec.garageRef" }"#,
    printcolumn = r#"{ "name": "quotas", "type": "string", "description": "quotas for this bucket", "jsonPath": ".spec.quotas" }"#,
    printcolumn = r#"{ "name": "status", "type": "string", "description": "bucket status", "jsonPath": ".status" }"#
)]
#[serde(rename_all = "camelCase")]
pub struct BucketSpec {
    pub garage_ref: String,
    pub quotas: BucketQuotas,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BucketQuotas {
    pub max_size: Option<Quantity>,
    pub max_object_count: Option<usize>,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
pub enum BucketStatus {
    #[default]
    Creating,
    Ready,
}
