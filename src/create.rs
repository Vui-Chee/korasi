use std::fs::read_to_string;

use aws_sdk_ec2::types::{InstanceType, KeyFormat, KeyType};
use base64::prelude::*;
use petname::{Generator, Petnames};

use super::ec2::{EC2Error, EC2Impl as EC2};
use super::util::UtilImpl as Util;

#[derive(Default)]
pub struct CreateCommand;

impl CreateCommand {
    pub async fn launch(
        &self,
        ec2: &EC2,
        machine: InstanceType,
        ami_id: String,
        pk_name: String,
        setup: String,
    ) -> Result<(), EC2Error> {
        let info = match ec2
            .create_key_pair(&pk_name, KeyType::Ed25519, KeyFormat::Pem)
            .await
        {
            Ok((info, material)) => {
                tracing::info!("Saving PK to file...");

                // Save private key.
                // Ignore error if file already exists.
                let _ = Util::write_secure(
                    &pk_name,
                    &format!("{}.pem", pk_name).into(),
                    material,
                    0o400,
                );

                Some(info)
            }
            Err(err) => {
                // NOTE: This assumes user already saved the private key locally.
                tracing::warn!("No key pair is created. Err = {}", err);
                let output = ec2.list_key_pair(&pk_name).await?;
                if !output.is_empty() {
                    tracing::info!(
                        "Reuse existing key pair: {:?}",
                        output[0].key_name.as_ref().unwrap()
                    );
                    Some(output[0].clone())
                } else {
                    tracing::error!("No instance is created since no existing key pair is found.");
                    None
                }
            }
        };

        let group = ec2.get_ssh_security_group().await?;
        tracing::info!("Security Group used = {:?}", group.group_id);

        let user_data = read_to_string(setup)
            .map(|data| BASE64_STANDARD.encode(data.as_bytes()))
            .ok();
        tracing::info!("User data: {:?}", user_data);

        let name = Petnames::default().generate_one(1, ":").unwrap();

        let _instance_id = ec2
            .create_instance(
                &name,
                &ami_id,
                machine,
                &info.unwrap(),
                vec![&group],
                user_data,
            )
            .await?;
        tracing::info!("Created instance with name = {}", name);

        Ok(())
    }
}
