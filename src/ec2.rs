use std::{net::Ipv4Addr, time::Duration};

use aws_sdk_ec2::{
    client::Waiters,
    error::ProvideErrorMetadata,
    types::{
        Filter, Instance, InstanceStateName, InstanceType, IpPermission, IpRange, KeyFormat,
        KeyPairInfo, KeyType, ResourceType, SecurityGroup, Tag, TagSpecification,
    },
    Client as EC2Client,
};

use crate::util::UtilImpl as Util;

/// Co-locate all common keys here for now till a flexible
/// configuration is needed.
pub const GLOBAL_TAG_FILTER: &str = "hpc-launcher";
pub const SSH_KEY_NAME: &str = "ec2-ssh-key";
pub const SSH_SECURITY_GROUP: &str = "allow-ssh";

#[derive(Clone)]
pub struct EC2Impl {
    /// AWS sdk client to access EC2 resources.
    pub client: EC2Client,

    /// Override default `GLOBAL_TAG_FILTER`.
    custom_tag: Option<String>,
}

impl EC2Impl {
    pub fn new(client: EC2Client, custom_tag: Option<String>) -> Self {
        EC2Impl { client, custom_tag }
    }

    pub fn create_tag(&self, res_type: ResourceType) -> TagSpecification {
        TagSpecification::builder()
            .set_resource_type(Some(res_type))
            .set_tags(Some(vec![Tag::builder()
                .set_key(Some("application".into()))
                .set_value(Some(
                    self.custom_tag
                        .clone()
                        .unwrap_or(GLOBAL_TAG_FILTER.to_string()),
                ))
                .build()]))
            .build()
    }

    pub async fn create_key_pair(
        &self,
        name: &str,
        key_type: KeyType,
        key_format: KeyFormat,
    ) -> Result<(KeyPairInfo, String), EC2Error> {
        tracing::info!("Creating key pair {name}");
        let output = self
            .client
            .create_key_pair()
            .key_name(name)
            .key_type(key_type)
            .key_format(key_format)
            .set_tag_specifications(Some(vec![self.create_tag(ResourceType::KeyPair)]))
            .send()
            .await?;
        tracing::info!("key pair output = {:?}", output);
        let info = KeyPairInfo::builder()
            .set_key_name(output.key_name)
            .set_key_fingerprint(output.key_fingerprint)
            .set_key_pair_id(output.key_pair_id)
            .build();
        let material = output
            .key_material
            .ok_or_else(|| EC2Error::new("Create Key Pair has no key material"))?;
        Ok((info, material))
    }

    pub async fn list_key_pair(&self, key_names: &str) -> Result<Vec<KeyPairInfo>, EC2Error> {
        let output = self
            .client
            .describe_key_pairs()
            .key_names(key_names)
            .set_filters(Some(vec![Filter::builder()
                .set_name(Some("tag:application".into()))
                .set_values(Some(vec![GLOBAL_TAG_FILTER.into()]))
                .build()]))
            .send()
            .await?;
        Ok(output.key_pairs.unwrap_or_default())
    }

    pub async fn delete_key_pair(&self, key_pair_id: &str) -> Result<(), EC2Error> {
        let key_pair_id: String = key_pair_id.into();
        tracing::info!("Deleting key pair {key_pair_id}");
        self.client
            .delete_key_pair()
            .key_pair_id(key_pair_id)
            .send()
            .await?;
        Ok(())
    }

    pub async fn create_security_group(
        &self,
        name: &str,
        description: &str,
    ) -> Result<SecurityGroup, EC2Error> {
        tracing::info!("Creating security group {name}");
        let create_output = self
            .client
            .create_security_group()
            .group_name(name)
            .description(description)
            .set_tag_specifications(Some(vec![self.create_tag(ResourceType::SecurityGroup)]))
            .send()
            .await
            .map_err(EC2Error::from)?;

        let group_id = create_output
            .group_id
            .ok_or_else(|| EC2Error::new("Missing security group id after creation"))?;

        let group = self
            .describe_security_group(&group_id)
            .await?
            .ok_or_else(|| {
                EC2Error::new(format!("Could not find security group with id {group_id}"))
            })?;

        tracing::info!("Created security group {name} as {group_id}");

        Ok(group)
    }

    /// Find a single security group, by name. Returns Err if multiple groups are found.
    pub async fn describe_security_group(
        &self,
        group_name: &str,
    ) -> Result<Option<SecurityGroup>, EC2Error> {
        let describe_output = self
            .client
            .describe_security_groups()
            .group_names(group_name)
            .set_filters(Some(vec![Filter::builder()
                .set_name(Some("tag:application".into()))
                .set_values(Some(vec![GLOBAL_TAG_FILTER.into()]))
                .build()]))
            .send()
            .await?;

        let mut groups = describe_output.security_groups.unwrap_or_default();

        match groups.len() {
            0 => Ok(None),
            1 => Ok(Some(groups.remove(0))),
            _ => Err(EC2Error::new(format!(
                "Expected single group for {group_name}"
            ))),
        }
    }

    /// Add an ingress rule to a security group explicitly allowing IPv4 address
    /// as {ip}/32 over TCP port 22.
    pub async fn authorize_security_group_ssh_ingress(
        &self,
        group_id: &str,
        ingress_ips: Vec<Ipv4Addr>,
    ) -> Result<(), EC2Error> {
        tracing::info!("Authorizing ingress for security group {group_id}");
        self.client
            .authorize_security_group_ingress()
            .group_id(group_id)
            .set_ip_permissions(Some(
                ingress_ips
                    .into_iter()
                    .map(|ip| {
                        IpPermission::builder()
                            .ip_protocol("tcp")
                            .from_port(22)
                            .to_port(22)
                            .ip_ranges(IpRange::builder().cidr_ip(format!("{ip}/32")).build())
                            .build()
                    })
                    .collect(),
            ))
            .send()
            .await?;
        Ok(())
    }

    pub async fn delete_security_group(&self, group_id: &str) -> Result<(), EC2Error> {
        tracing::info!("Deleting security group {group_id}");
        self.client
            .delete_security_group()
            .group_id(group_id)
            .send()
            .await?;
        Ok(())
    }

    pub async fn create_instances<'a>(
        &self,
        instance_name: &str,
        image_id: &'a str,
        instance_type: InstanceType,
        key_pair: &'a KeyPairInfo,
        security_groups: Vec<&'a SecurityGroup>,
        user_data: Option<String>,
    ) -> Result<Vec<String>, EC2Error> {
        let run_instances = self
            .client
            .run_instances()
            .image_id(image_id)
            .instance_type(instance_type)
            .key_name(
                key_pair
                    .key_name()
                    .ok_or_else(|| EC2Error::new("Missing key name when launching instance"))?,
            )
            .set_security_group_ids(Some(
                security_groups
                    .iter()
                    .filter_map(|sg| sg.group_id.clone())
                    .collect(),
            ))
            .set_user_data(user_data)
            .set_tag_specifications(Some(vec![self.create_tag(ResourceType::Instance)]))
            .min_count(1)
            .max_count(1)
            .send()
            .await?;

        if run_instances.instances().is_empty() {
            return Err(EC2Error::new("Failed to create instance"));
        }

        let mut instance_ids = vec![];
        for i in run_instances.instances() {
            let instance_id = i.instance_id().unwrap();
            let response = self
                .client
                .create_tags()
                .resources(instance_id)
                .tags(Tag::builder().key("Name").value(instance_name).build())
                .send()
                .await;

            match response {
                Ok(_) => {
                    tracing::info!("Created {instance_id} and applied tags.");
                    instance_ids.push(instance_id.to_string());
                }
                Err(err) => {
                    tracing::info!("Error applying tags to {instance_id}: {err:?}");
                    return Err(err.into());
                }
            }
        }

        Ok(instance_ids)
    }

    /// Wait for an instance to be ready and status ok (default wait 60 seconds)
    pub async fn wait_for_instance_ready(
        &self,
        instance_id: &str,
        duration: Option<Duration>,
    ) -> Result<(), EC2Error> {
        self.client
            .wait_until_instance_status_ok()
            .instance_ids(instance_id)
            .wait(duration.unwrap_or(Duration::from_secs(60)))
            .await?;
        Ok(())
    }

    /// List instances that are "active" (non-terminated) and are tagged
    /// by this tool.
    ///
    /// If statuses is an empty `Vec`, return all non-terminated instances as the default.
    pub async fn describe_instance(
        &self,
        mut statuses: Vec<InstanceStateName>,
    ) -> Result<Vec<Instance>, EC2Error> {
        let non_terminated = vec![
            InstanceStateName::Pending,
            InstanceStateName::Running,
            InstanceStateName::ShuttingDown,
            InstanceStateName::Stopping,
            InstanceStateName::Stopped,
        ];
        if statuses.is_empty() {
            statuses = non_terminated;
        }
        let response = self
            .client
            .describe_instances()
            .set_filters(Some(vec![
                Filter::builder()
                    .set_name(Some("tag:application".into()))
                    .set_values(Some(vec![GLOBAL_TAG_FILTER.into()]))
                    .build(),
                Filter::builder()
                    .set_name(Some("instance-state-name".into()))
                    .set_values(Some(statuses.into_iter().map(|s| s.to_string()).collect()))
                    .build(),
            ]))
            .send()
            .await?;

        let instances: Vec<_> = response
            .reservations()
            .iter()
            .flat_map(|r| r.instances().to_owned())
            .collect();

        Ok(instances)
    }

    pub async fn start_instances(&self, instance_id: &str) -> Result<(), EC2Error> {
        tracing::info!("Starting instance {instance_id}");

        let mut starter = self.client.start_instances();
        for id in instance_id.split(",") {
            starter = starter.instance_ids(id);
        }
        starter.send().await?;

        tracing::info!("Started instance.");

        Ok(())
    }

    pub async fn stop_instances(&self, instance_ids: &str, wait: bool) -> Result<(), EC2Error> {
        tracing::info!("Stopping instance {instance_ids}");

        let mut stopper = self.client.stop_instances();
        for id in instance_ids.split(",") {
            stopper = stopper.instance_ids(id);
        }
        stopper.send().await?;

        if wait {
            self.wait_for_instance_stopped(instance_ids, None).await?;
            tracing::info!("Stopped instance.");
        }

        Ok(())
    }

    pub async fn reboot_instance(&self, instance_id: &str) -> Result<(), EC2Error> {
        tracing::info!("Rebooting instance {instance_id}");

        self.client
            .reboot_instances()
            .instance_ids(instance_id)
            .send()
            .await?;

        Ok(())
    }

    pub async fn wait_for_instance_stopped(
        &self,
        instance_ids: &str,
        duration: Option<Duration>,
    ) -> Result<(), EC2Error> {
        let mut waiter = self.client.wait_until_instance_stopped();
        for id in instance_ids.split(",") {
            waiter = waiter.instance_ids(id);
        }
        waiter
            .wait(duration.unwrap_or(Duration::from_secs(90)))
            .await?;

        Ok(())
    }

    pub async fn delete_instances(&self, instance_ids: &str, wait: bool) -> Result<(), EC2Error> {
        tracing::info!("Deleting instance with id {:?}", instance_ids);

        self.stop_instances(instance_ids, wait).await?;

        let mut terminator = self.client.terminate_instances();
        for id in instance_ids.split(",") {
            terminator = terminator.instance_ids(id);
        }
        terminator.send().await?;

        if wait {
            self.wait_for_instance_terminated(instance_ids).await?;
            tracing::info!("Terminated instance with ids {:?}", instance_ids);
        }

        Ok(())
    }

    async fn wait_for_instance_terminated(&self, instance_ids: &str) -> Result<(), EC2Error> {
        let mut waiter = self.client.wait_until_instance_terminated();
        for id in instance_ids.split(",") {
            waiter = waiter.instance_ids(id);
        }
        waiter.wait(Duration::from_secs(60)).await?;
        Ok(())
    }

    /// Add new local IP to inbound security group.
    ///
    /// Local IPs can rotate or if you change to a different location.
    async fn update_inbound_ip(&self, group_id: &str) -> Result<(), EC2Error> {
        let check_ip = Util::do_get("https://checkip.amazonaws.com").await?;
        tracing::info!("Current IP address = {}", check_ip);

        let current_ip_address: Ipv4Addr = check_ip.trim().parse().map_err(|e| {
            EC2Error::new(format!(
                "Failed to convert response {} to IP Address: {e:?}",
                check_ip
            ))
        })?;

        if let Err(err) = self
            .authorize_security_group_ssh_ingress(group_id, vec![current_ip_address])
            .await
        {
            tracing::warn!("Most likely inbound rule already exists. Err = {err}");
        };

        Ok(())
    }

    /// Call this function to update local IP in inbound group.
    pub async fn get_ssh_security_group(&self) -> Result<SecurityGroup, EC2Error> {
        let group = match self
            .create_security_group(
                SSH_SECURITY_GROUP,
                "Enables ssh into instance from your IP.",
            )
            .await
        {
            Ok(grp) => grp,
            Err(err) => {
                // Try to find existing group (if any).
                let res = self.describe_security_group(SSH_SECURITY_GROUP).await?;

                if res.is_none() {
                    return Err(err);
                }

                res.unwrap()
            }
        };

        self.update_inbound_ip(group.group_id.as_ref().unwrap())
            .await?;

        Ok(group)
    }
}

#[derive(Debug)]
pub struct EC2Error(String);
impl EC2Error {
    pub fn new(value: impl Into<String>) -> Self {
        EC2Error(value.into())
    }

    pub fn _add_message(self, message: impl Into<String>) -> Self {
        EC2Error(format!("{}: {}", message.into(), self.0))
    }
}

impl<T: ProvideErrorMetadata> From<T> for EC2Error {
    fn from(value: T) -> Self {
        EC2Error(format!(
            "{}: {}",
            value
                .code()
                .map(String::from)
                .unwrap_or("unknown code".into()),
            value
                .message()
                .map(String::from)
                .unwrap_or("missing reason (most likely profile credentials not set)".into()),
        ))
    }
}

impl std::error::Error for EC2Error {}

impl std::fmt::Display for EC2Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
