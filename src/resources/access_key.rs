use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A bucket in a garage instance
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "AccessKey",
    group = "deuxfleurs.fr",
    version = "v0alpha",
    status = "AccessKeyStatus",
    namespaced,
    printcolumn = r#"{ "name": "bucket", "type": "string", "description": "owning bucket instance", "jsonPath": ".spec.bucketRef" }"#,
    printcolumn = r#"{ "name": "permissions", "type": "string", "description": "permissions for this bucket", "jsonPath": ".state.permissionsFriendly" }"#,
    printcolumn = r#"{ "name": "status", "type": "string", "description": "bucket status", "jsonPath": ".status.state" }"#
)]
#[serde(rename_all = "camelCase")]
pub struct AccessKeySpec {
    pub bucket_ref: String,
    pub garage_ref: String,
    pub permissions: AccessKeyPermissions,

    /// Optionally set the name of the generated secret.
    /// The default is NAME.BUCKET.GARAGE.key
    pub secret_ref: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
pub struct AccessKeyPermissions {
    pub read: Option<bool>,
    pub write: Option<bool>,
    pub owner: Option<bool>,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccessKeyStatus {
    pub state: AccessKeyState,
    pub permissions_friendly: String,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
pub enum AccessKeyState {
    #[default]
    Creating,
    Ready,
}
