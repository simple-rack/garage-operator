use k8s_openapi::api::core::v1::SecretReference;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::NamespacedReference;

/// Specification for an access key for a particular bucket
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    kind = "AccessKey",
    group = "deuxfleurs.fr",
    version = "v0alpha",
    status = "AccessKeyStatus",
    doc = "An access key for a particular bucket",
    namespaced,
    printcolumn = r#"{ "name": "bucket", "type": "string", "description": "owning bucket instance", "jsonPath": ".spec.bucketRef" }"#,
    printcolumn = r#"{ "name": "permissions", "type": "string", "description": "permissions for this bucket", "jsonPath": ".state.permissionsFriendly" }"#,
    printcolumn = r#"{ "name": "status", "type": "string", "description": "bucket status", "jsonPath": ".status.state" }"#
)]
#[serde(rename_all = "camelCase")]
pub struct AccessKeySpec {
    /// A reference to an existing garage.
    // TODO: Is there no way that we could omit this?
    pub garage_ref: NamespacedReference,

    /// A reference to an existing bucket.
    pub bucket_ref: NamespacedReference,

    /// Permissions associated with the key.
    pub permissions: AccessKeyPermissions,

    /// Set the location of the generated secret.
    pub secret_ref: SecretReference,
}

/// The required permissions for this access key
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(default)]
pub struct AccessKeyPermissions {
    /// Allow reading files from a bucket.
    pub read: bool,

    /// Allow writing files to a bucket.
    pub write: bool,

    /// Allow modifying the configuration of a bucket.
    pub owner: bool,
}

/// The status of an access key.
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccessKeyStatus {
    /// The current state of the key
    pub state: AccessKeyState,

    /// A friendly representation of the permissions granted to this key.
    ///
    /// Format is RWO, where R is read, W is write, and O is owner. Missing permissions
    /// show as -.
    pub permissions_friendly: String,
}

/// The possible states of an access key
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
pub enum AccessKeyState {
    /// The access key is in the process of being created.
    #[default]
    Creating,

    /// The access key is ready for use.
    Ready,
}
