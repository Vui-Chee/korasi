use std::fs::read_to_string;

use aws_sdk_ec2::types::{InstanceType, KeyPairInfo};
use base64::prelude::*;
use petname::{Generator, Petnames};

use super::ec2::{EC2Error, EC2Impl as EC2};

#[derive(Default)]
pub struct CreateCommand;

impl CreateCommand {
    pub async fn launch(
        &self,
        ec2: &EC2,
        machine: InstanceType,
        ami_id: String,
        info: KeyPairInfo,
        setup: String,
    ) -> Result<(), EC2Error> {
        let group = ec2.get_ssh_security_group().await?;
        tracing::info!("Security Group used = {:?}", group.group_id);

        let user_data = read_to_string(setup)
            .map(|data| BASE64_STANDARD.encode(data.as_bytes()))
            .ok();
        tracing::info!("User data: {:?}", user_data);

        let name = Petnames::default().generate_one(1, ":").unwrap();

        let _instance_ids = ec2
            .create_instances(&name, &ami_id, machine, &info, vec![&group], user_data)
            .await?;
        tracing::info!("Created instance with name = {}", name);

        Ok(())
    }
}
