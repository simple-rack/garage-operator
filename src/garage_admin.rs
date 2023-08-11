use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use kube::ResourceExt;
use kube_quantity::{ParseQuantityError, ParsedQuantity};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};

use crate::{
    garage_admin::client::types::CreateBucketBody,
    resources::{AccessKeySpec, BucketSpec, Garage},
    Result,
};

use self::client::types::{
    AddKeyBody, AllowBucketKeyBody, AllowBucketKeyBodyPermissions, KeyInfo, LayoutVersion,
    NodeClusterInfo, UpdateBucketBody, UpdateBucketBodyQuotas,
};

/// Autogenerated client for the garage admin API using its corresponding openapi spec.
pub mod client {
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

        let admin_port = garage
            .spec
            .config
            .as_ref()
            .map(|c| c.ports.admin)
            .unwrap_or(3903);
        let url = format!(
            "http://{}.{}.svc.cluster.local:{}/v0",
            garage.prefixed_name("admin"),
            garage.namespace().unwrap(),
            admin_port,
        );

        Ok(GarageAdmin {
            garage,
            client: client::Client::new_with_client(&url, client),
        })
    }

    pub async fn get_access_keys(&self) -> Result<HashSet<String>> {
        let access_keys = self.client.list_keys().await?;

        Ok(access_keys
            .into_inner()
            .into_iter()
            .map(|ak| ak.id)
            .collect())
    }

    /// Get all available bucket IDs
    pub async fn get_buckets(&self) -> Result<HashSet<String>> {
        let buckets = self.client.list_buckets().await?;

        Ok(buckets.into_inner().into_iter().map(|b| b.id).collect())
    }

    pub async fn layout_instance(&self) -> Result<()> {
        // Get the current status of the instance, failing if it is unhealthy
        let nodes = self.client.get_nodes().await?.into_inner();

        // If the node has been laid out already, then skip
        // TODO: Write out a message
        let node = nodes.node;
        if nodes.layout.version != 0 || nodes.layout.staged_role_changes.contains_key(&node) {
            return Ok(());
        }

        // Attempt to layout the instance
        let _layout = self
            .client
            .add_layout(&HashMap::from([(
                node,
                NodeClusterInfo {
                    capacity: Some(1),
                    tags: vec![
                        "owned-by/garage-operator".into(),
                        format!("garage-instance/{}", self.garage.name_any()),
                    ],
                    zone: self
                        .garage
                        .spec
                        .config
                        .as_ref()
                        .map(|c| c.region.clone())
                        .unwrap_or("garage".into()),
                },
            )]))
            .await?;

        // Actually apply the layout
        let _apply = self
            .client
            .apply_layout(&LayoutVersion { version: Some(1) })
            .await?;

        // TODO: Write out a message
        Ok(())
    }

    pub async fn create_access_key(&self, name: String) -> Result<KeyInfo> {
        // Create the bucket first
        let create_response = self
            .client
            .add_key(&AddKeyBody { name: Some(name) })
            .await?;

        Ok(create_response.into_inner())
    }

    pub async fn create_bucket(&self, name: String) -> Result<String> {
        // Create the bucket first
        let create_response = self
            .client
            .create_bucket(&CreateBucketBody {
                global_alias: Some(name),
                local_alias: None,
            })
            .await?;

        // TODO: Apparently the ID is optional?
        Ok(create_response.into_inner().id.unwrap())
    }

    pub async fn delete_access_key(&self, id: &str) -> Result<()> {
        // Delete the access key
        let _delete_response = self.client.delete_key(id).await?;

        Ok(())
    }

    pub async fn delete_bucket(&self, id: &str) -> Result<()> {
        // Delete the bucket
        let _delete_response = self.client.delete_bucket(id).await?;

        Ok(())
    }

    /// Updates the access permissions for a key
    /// Note: Garage has decided that this should be two different endpoints
    /// ¯\_(ツ)_/¯
    pub async fn update_access_key(
        &self,
        bucket_id: &str,
        access_key_id: &str,
        access_key: &AccessKeySpec,
    ) -> Result<()> {
        let _update_allow = self
            .client
            .allow_bucket_key(&AllowBucketKeyBody {
                access_key_id: access_key_id.into(),
                bucket_id: bucket_id.into(),
                permissions: AllowBucketKeyBodyPermissions {
                    owner: access_key.permissions.owner.unwrap_or_default(),
                    read: access_key.permissions.read.unwrap_or_default(),
                    write: access_key.permissions.write.unwrap_or_default(),
                },
            })
            .await?;

        let _update_deny = self
            .client
            .allow_bucket_key(&AllowBucketKeyBody {
                access_key_id: access_key_id.into(),
                bucket_id: bucket_id.into(),
                permissions: AllowBucketKeyBodyPermissions {
                    owner: !access_key.permissions.owner.unwrap_or_default(),
                    read: !access_key.permissions.read.unwrap_or_default(),
                    write: !access_key.permissions.write.unwrap_or_default(),
                },
            })
            .await?;

        Ok(())
    }

    pub async fn update_bucket(&self, bucket_id: &str, bucket: &BucketSpec) -> Result<()> {
        // Then configure any quotas
        // TODO: Handle errors gracefully here
        let max_size = bucket.quotas.max_size.as_ref().map(|q| {
            let quantity: Result<ParsedQuantity, ParseQuantityError> = q.try_into();
            quantity
                .expect("lmao invalid quantity gitgud")
                .to_bytes_i64()
                .unwrap()
        });

        // Only apply quotas if at least one of the options is specified
        let quota_conf = &bucket.quotas;
        let quotas = if quota_conf.max_object_count.is_some() || quota_conf.max_size.is_some() {
            Some(UpdateBucketBodyQuotas {
                max_objects: quota_conf.max_object_count.map(|m| m as i64),
                max_size: max_size,
            })
        } else {
            None
        };

        let _update = self
            .client
            .update_bucket(
                bucket_id,
                &UpdateBucketBody {
                    quotas,
                    website_access: None,
                },
            )
            .await?;

        Ok(())
    }
}
