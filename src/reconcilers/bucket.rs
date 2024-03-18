use std::{sync::Arc, time::Duration};

use kube::{
    api::{ListParams, Patch, PatchParams},
    runtime::controller::Action,
    Api, ResourceExt as _,
};
use serde_json::json;
use tracing::info;

use crate::{
    resources::{AccessKey, Bucket, BucketState, BucketStatus, Garage},
    Error,
};

use super::{CommonContext, Reconcile};

pub struct BucketContext {
    pub common: Arc<CommonContext>,
    pub owner: Garage,
}

#[async_trait::async_trait]
impl Reconcile for Bucket {
    type Context = BucketContext;

    async fn reconcile(&self, context: Arc<Self::Context>) -> Result<Action, Error> {
        info!(
            "Reconciling bucket '{}' of garage '{}/{}'",
            self.name_any(),
            self.spec.garage_ref.namespace,
            self.spec.garage_ref.name,
        );

        // Grab a handle to the admin API for querying the running instance
        let admin = context.owner.create_admin(context.common.clone()).await?;

        // Extract needed info from this bucket
        let name = self.name_any();
        let namespace = self
            .namespace()
            .ok_or_else(|| Error::IllegalBucket(name.clone(), "missing namespace".into()))?;

        // Grab a handle to k8s resources
        let bucket_handle = Api::<Bucket>::namespaced(context.common.client.clone(), &namespace);
        let access_key_handle = Api::<AccessKey>::all(context.common.client.clone());

        // Get the last known status of this bucket, using the default if not present
        let status = self.status.clone().unwrap_or_default();

        // Deploy all resources needed by this bucket
        self.deploy_resources(context.clone()).await?;

        // Handle all possible states for this bucket
        let (requeue, next_status): (Duration, BucketStatus) = match status.state {
            // The bucket needs to be either created or linked up with an existing bucket
            BucketState::Creating => {
                // Grab the bucket's ID from garage
                let id = if let Some(b) = admin.get_bucket_by_name(&name).await? {
                    b.id.unwrap()
                } else {
                    // The bucket doesn't already exist, so create it now
                    admin.create_bucket(&name).await?.id.unwrap()
                };

                // Save the ID and get ready to configure
                (
                    Duration::from_secs(2),
                    BucketStatus {
                        id,
                        state: BucketState::Configuring,
                    },
                )
            }

            // Apply quotas to our bucket
            BucketState::Configuring => {
                // Always overwrite with our source of truth
                admin
                    .set_bucket_quotas(&status.id, &self.spec.quotas)
                    .await?;

                (
                    Duration::from_secs(1),
                    BucketStatus {
                        id: status.id,
                        state: BucketState::Ready,
                    },
                )
            }

            // Apply all access keys once we are ready
            BucketState::Ready => {
                (
                    Duration::from_secs(60 * 60),
                    BucketStatus {
                        id: status.id,
                        state: BucketState::Ready,
                    },
                )
            }

            // If we have encountered an error, try to start over in 15 seconds
            BucketState::Errored => (Duration::from_secs(15), BucketStatus::default()),
        };

        let new_status = Patch::Apply(json!({
            "apiVersion": "deuxfleurs.fr/v0alpha",
            "kind": "Bucket",
            "status": next_status,
        }));
        let ps = PatchParams::apply("garage-operator").force(); // TODO: Why is this force?
        let _o = bucket_handle.patch_status(&name, &ps, &new_status).await?;

        Ok(Action::requeue(requeue))
    }

    async fn deploy_resources(&self, _context: Arc<Self::Context>) -> Result<(), Error> {
        // Buckets do not require any k8s resources
        Ok(())
    }
}
