use std::time::Duration;

use http::StatusCode;
use kube::ResourceExt;
use kube_quantity::ParsedQuantity;
use progenitor_client::ResponseValue;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};

use crate::{
    admin_api::client::types::{GetKeyShowSecretKey, UpdateBucketBody, UpdateBucketBodyQuotas},
    resources::{AccessKey, Bucket, BucketQuotas, Garage},
    Error, Result,
};

use self::client::types::{
    AddKeyBody, AllowBucketKeyBody, AllowBucketKeyBodyPermissions, BucketInfo, CreateBucketBody,
    KeyInfo, LayoutVersion, NodeRoleChange, NodeRoleUpdate,
};

/// Autogenerated client for the garage admin API using its corresponding openapi spec.
mod client {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/garage-admin-client.rs"));
}

pub struct GarageAdmin<'a> {
    garage: &'a Garage,
    client: client::Client,
}

impl<'a> GarageAdmin<'a> {
    pub fn with_secret(garage: &'a Garage, token: &str) -> Result<GarageAdmin<'a>> {
        // All requests must be authenticated using bearer auth
        let headers = {
            let mut headers = HeaderMap::new();
            let mut auth = HeaderValue::from_str(&format!("Bearer {token}")).unwrap();
            auth.set_sensitive(true);

            headers.insert(AUTHORIZATION, auth);
            headers
        };

        // Use a client to handle setting common request parameters
        // TODO: Handle error here nicely
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .default_headers(headers)
            .build()
            .unwrap();

        let admin_port = garage.spec.config.ports.admin;
        let url = format!(
            "http://{}.{}.svc.cluster.local:{}/v1",
            garage.prefixed_name("api"),
            garage.namespace().unwrap(),
            admin_port,
        );

        Ok(GarageAdmin {
            garage,
            client: client::Client::new_with_client(&url, client),
        })
    }

    pub async fn layout_instance(&self, capacity: i64) -> Result<bool> {
        // Get the current status of the instance, failing if it is unhealthy
        let nodes = self.client.get_nodes().await?.into_inner();

        // If the node has been laid out already, then skip
        // TODO: Write out a message
        let node_id = nodes.node;
        if nodes.layout.version != 0 {
            return Ok(true);
        }

        // Add a layout request if we did not already
        let staged = nodes
            .layout
            .staged_role_changes
            .iter()
            .any(|change| match change {
                NodeRoleChange::Update(NodeRoleUpdate { id, .. }) => *id == node_id,
                _ => false,
            });

        if !staged {
            let _layout = self
                .client
                .add_layout(&vec![NodeRoleChange::Update(NodeRoleUpdate {
                    capacity: Some(capacity),
                    id: node_id,
                    tags: vec![
                        "owned-by/garage-operator".into(),
                        format!("garage-instance/{}", self.garage.name_any()),
                    ],
                    zone: self.garage.spec.config.region.clone(),
                })])
                .await?;
        }

        // Actually apply the layout
        let _apply = self
            .client
            .apply_layout(&LayoutVersion { version: 1 })
            .await?;

        // TODO: Write out a message
        Ok(false)
    }
}

// Bucket related actions
impl GarageAdmin<'_> {
    /// Create a bucket
    pub async fn create_bucket(&self, name: &str) -> Result<BucketInfo> {
        self.client
            .create_bucket(&CreateBucketBody {
                global_alias: Some(name.to_string()),
                local_alias: None,
            })
            .await
            .map(ResponseValue::into_inner)
            .map_err(Error::NetworkError)
    }

    /// Fetches bucket information from garage by its name, if it exists
    pub async fn get_bucket_by_name(&self, name: &str) -> Result<Option<BucketInfo>> {
        match self
            .client
            .get_bucket_info(Some(name), None)
            .await
            .map(ResponseValue::into_inner)
        {
            // The admin API returns an empty bucket if you ask for one that does not exist...
            Ok(BucketInfo { id: None, .. }) => Ok(None),
            Ok(bucket) => Ok(Some(bucket)),

            // If it errors, it could be because it doesn't exist
            Err(e) => {
                if matches!(e.status(), Some(StatusCode::NOT_FOUND)) {
                    Ok(None)
                } else {
                    Err(Error::NetworkError(e))
                }
            }
        }
    }

    /// Set the quotas for a bucket
    pub async fn set_bucket_quotas(&self, id: &str, quotas: &BucketQuotas) -> Result<()> {
        let max_size = quotas
            .max_size
            .as_ref()
            .and_then(|max_size| ParsedQuantity::try_from(max_size).unwrap().to_bytes_i64()); // TODO: Remove unwrap

        self.client
            .update_bucket(
                id,
                &UpdateBucketBody {
                    quotas: Some(UpdateBucketBodyQuotas {
                        max_objects: quotas.max_object_count.map(|m| m as i64),
                        max_size,
                    }),
                    website_access: None,
                },
            )
            .await?;

        Ok(())
    }
}

// Access key related ops
impl GarageAdmin<'_> {
    /// Create a new API key
    pub async fn create_key(&self, name: &str) -> Result<KeyInfo> {
        self.client
            .add_key(&AddKeyBody {
                name: Some(name.to_string()),
            })
            .await
            .map(ResponseValue::into_inner)
            .map_err(Error::NetworkError)
    }

    /// Look up a key by its name
    pub async fn get_key_by_name(
        &self,
        name: &str,
        fetch_secret: bool,
    ) -> Result<Option<KeyInfo>, Error> {
        // Ask garage for the key
        match self
            .client
            .get_key(
                None,
                Some(name),
                Some(if fetch_secret {
                    GetKeyShowSecretKey::True
                } else {
                    GetKeyShowSecretKey::False
                }),
            )
            .await
            .map(ResponseValue::into_inner)
        {
            // This API is somewhat painful
            Ok(KeyInfo {
                access_key_id: None,
                ..
            }) => Ok(None),
            Ok(key) => {
                if key.name == Some(name.to_string()) {
                    Ok(Some(key))
                } else {
                    Ok(None)
                }
            }

            // If it errors, it could be because it doesn't exist
            Err(e) => {
                if matches!(e.status(), Some(StatusCode::BAD_REQUEST)) {
                    Ok(None)
                } else {
                    Err(Error::NetworkError(e))
                }
            }
        }
    }

    /// Allow a key to be used for a specific bucket
    pub async fn allow_key_for_bucket(&self, key: &AccessKey, bucket: &Bucket) -> Result<()> {
        self.client
            .allow_bucket_key(&AllowBucketKeyBody {
                access_key_id: key.status.as_ref().unwrap().id.to_string(),
                bucket_id: bucket.status.as_ref().unwrap().id.to_string(),
                permissions: AllowBucketKeyBodyPermissions {
                    owner: key.spec.permissions.owner,
                    read: key.spec.permissions.read,
                    write: key.spec.permissions.write,
                },
            })
            .await?;

        Ok(())
    }
}
