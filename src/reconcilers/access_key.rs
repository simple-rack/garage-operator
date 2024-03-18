use std::{collections::BTreeMap, sync::Arc, time::Duration};

use k8s_openapi::api::core::v1::Secret;
use kube::{
    api::{Patch, PatchParams},
    runtime::controller::Action,
    Api, Resource as _, ResourceExt as _,
};
use serde_json::json;
use tracing::info;

use crate::{
    meta,
    resources::{AccessKey, AccessKeyState, AccessKeyStatus, Bucket, Garage},
    Error,
};

use super::{CommonContext, Reconcile};

pub struct AccessKeyContext {
    pub common: Arc<CommonContext>,
    pub owner: Garage,
    pub bucket: Bucket,
}

#[async_trait::async_trait]
impl Reconcile for AccessKey {
    type Context = AccessKeyContext;

    async fn reconcile(&self, context: Arc<Self::Context>) -> Result<Action, Error> {
        info!(
            "Reconciling access key '{}' of garage '{}/{}' and bucket '{}/{}'",
            self.name_any(),
            self.spec.garage_ref.namespace,
            self.spec.garage_ref.name,
            self.spec.bucket_ref.namespace,
            self.spec.bucket_ref.name,
        );

        // Grab a handle to the admin API for querying the running instance
        let admin = context.owner.create_admin(context.common.clone()).await?;

        // Extract needed info from this bucket
        let name = self.name_any();
        let namespace = self
            .namespace()
            .ok_or_else(|| Error::IllegalAccessKey(name.clone(), "missing namespace".into()))?;

        // Grab a handle to k8s resources
        let access_key_handle =
            Api::<AccessKey>::namespaced(context.common.client.clone(), &namespace);

        // Get the last known status of this bucket, using the default if not present
        let status = self.status.clone().unwrap_or_default();

        let (requeue, next_status) = match status.state {
            AccessKeyState::Creating => {
                // Grab the key's ID from garage
                let id = if let Some(k) = admin.get_key_by_name(&name, false).await? {
                    k.access_key_id.unwrap()
                } else {
                    // The bucket doesn't already exist, so create it now
                    admin.create_key(&name).await?.access_key_id.unwrap()
                };

                (
                    Duration::from_secs(2),
                    AccessKeyStatus {
                        id,
                        state: AccessKeyState::Configuring,
                        permissions_friendly: self.spec.permissions.to_string(),
                    },
                )
            }

            // Link the access key to the correct bucket and update permissions
            AccessKeyState::Configuring => {
                admin.allow_key_for_bucket(self, &context.bucket).await?;

                (
                    Duration::from_secs(2),
                    AccessKeyStatus {
                        id: status.id,
                        state: AccessKeyState::Ready,
                        permissions_friendly: status.permissions_friendly,
                    },
                )
            }

            // Continually write the secret in case it gets regenerated
            AccessKeyState::Ready => {
                self.deploy_resources(context.clone()).await?;

                (
                    Duration::from_secs(60 * 60),
                    AccessKeyStatus {
                        id: status.id,
                        state: AccessKeyState::Ready,
                        permissions_friendly: status.permissions_friendly,
                    },
                )
            }

            // If we have encountered an error, try to start over in 15 seconds
            AccessKeyState::Errored => (Duration::from_secs(15), AccessKeyStatus::default()),
        };

        // always overwrite status object with what we saw
        let new_status = Patch::Apply(json!({
            "apiVersion": "deuxfleurs.fr/v0alpha",
            "kind": "AccessKey",
            "status": next_status,
        }));
        let ps = PatchParams::apply("garage-operator").force(); // TODO: Why is this force?
        let _o = access_key_handle
            .patch_status(&name, &ps, &new_status)
            .await?;

        Ok(Action::requeue(requeue))
    }

    // The only resource needed for an access key is the secret containing the s3 info
    async fn deploy_resources(&self, context: Arc<Self::Context>) -> Result<(), Error> {
        // Get needed info
        let name = self.name_any();
        let namespace = self
            .namespace()
            .ok_or_else(|| Error::IllegalAccessKey(name.clone(), "missing namespace".into()))?;
        let owner = self.controller_owner_ref(&()).unwrap();
        let secret_id = self
            .spec
            .secret_ref
            .name
            .clone()
            .unwrap_or(format!("{}.{}.key", name, self.spec.bucket_ref.name));

        let admin = context.owner.create_admin(context.common.clone()).await?;
        let secrets_handle = Api::<Secret>::namespaced(context.common.client.clone(), &namespace);

        // Fetch the current secret from garage
        let key = admin.get_key_by_name(&name, true).await?.unwrap();

        // Write out the secret to k8s
        let garage_config = &context.owner.spec.config;
        let secret = Secret {
            metadata: meta! {
                owners: vec![owner.clone()],
                name: Some(secret_id.clone())
            },
            string_data: Some(BTreeMap::from([
                ("AWS_ACCESS_KEY_ID".into(), key.access_key_id.unwrap()),
                (
                    "AWS_SECRET_ACCESS_KEY".into(),
                    key.secret_access_key.unwrap(),
                ),
                ("AWS_DEFAULT_REGION".into(), garage_config.region.clone()),
                (
                    "AWS_ENDPOINT_URL".into(),
                    format!(
                        "http://{}.{}.svc.cluster.local:{}",
                        context.owner.prefixed_name("api"),
                        context.owner.namespace().unwrap(),
                        garage_config.ports.s3_api
                    ),
                ),
            ])),

            ..Default::default()
        };

        secrets_handle
            .patch(
                &secret_id,
                &PatchParams::apply("garage-operator"),
                &Patch::Apply(secret),
            )
            .await?;

        Ok(())
    }
}
