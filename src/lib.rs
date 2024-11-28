pub mod create;
pub mod ec2;
pub mod opt;
pub mod ssh;
pub mod util;

use anyhow::Context;
use aws_config::{
    self, meta::region::RegionProviderChain, timeout::TimeoutConfig, BehaviorVersion,
};
use aws_sdk_ec2::types::{InstanceStateName, InstanceType};
use aws_types::{region::Region, SdkConfig as AwsSdkConfig};
use inquire::{Select, Text};
use termion::raw::IntoRawMode;
use tokio::time::Duration;

use create::CreateCommand;
use ec2::{EC2Impl as EC2, SSH_KEY_NAME, SSH_SECURITY_GROUP};
use opt::{Commands, Opt};
use ssh::Session;
use util::{ids_to_str, multi_select_instances, select_instance, UtilImpl as Util};

/// Loads an AWS config from default environments.
pub async fn load_config(
    region: Option<String>,
    profile_name: Option<String>,
    operation_timeout: Option<Duration>,
) -> AwsSdkConfig {
    tracing::info!("loading config for the region {:?}", region);

    // if region is None, it automatically detects iff it's running inside the EC2 instance
    let reg_provider = RegionProviderChain::first_try(region.map(Region::new))
        .or_default_provider()
        .or_else(Region::new("ap-southeast-1"));

    let mut builder = TimeoutConfig::builder().connect_timeout(Duration::from_secs(5));
    if let Some(to) = &operation_timeout {
        if !to.is_zero() {
            builder = builder.operation_timeout(*to);
        }
    }
    let timeout_cfg = builder.build();

    let mut cfg = aws_config::defaults(BehaviorVersion::v2024_03_28())
        .region(reg_provider)
        .profile_name(profile_name.as_ref().unwrap_or(&"default".to_string()))
        .timeout_config(timeout_cfg);
    if let Some(p) = profile_name {
        tracing::info!("loading the aws profile '{p}'");
        cfg = cfg.profile_name(p);
    }

    cfg.load().await
}

pub async fn run(opts: Opt) -> anyhow::Result<()> {
    let Opt {
        profile,
        region,
        ssh_key,
        tag,
        ..
    } = opts;

    let ssh_path = std::env::var("HOME")
        .map(|h| {
            if let Some(ssh_key) = ssh_key {
                ssh_key
            } else {
                format!("{}/.ssh/{SSH_KEY_NAME}.pem", h)
            }
        })
        .unwrap();

    let shared_config = load_config(Some(region), Some(profile), None).await;
    let client = aws_sdk_ec2::Client::new(&shared_config);
    let ec2 = EC2::new(client, tag);

    let info = Util::create_or_get_keypair(&ec2, ssh_path.clone()).await?;
    tracing::info!("Using SSH key at = {}", ssh_path);

    match opts.commands {
        Commands::Create { ami_id } => {
            let machine: InstanceType =
                Select::new("Select the machine type:", InstanceType::values().to_vec())
                    .prompt()
                    .unwrap()
                    .into();
            tracing::info!("Launching {machine} instance...");
            CreateCommand
                .launch(&ec2, machine, ami_id, info.unwrap(), "start_up.sh".into())
                .await?;
        }
        Commands::List => {
            let res = ec2.describe_instance(vec![]).await.unwrap();
            if res.is_empty() {
                tracing::warn!("There are no active instances.");
                return Ok(());
            }
            for (i, instance) in res.iter().enumerate() {
                let tags = instance.tags();
                let mut name = "";
                for t in tags {
                    if t.key() == Some("Name") {
                        name = t.value().unwrap();
                    }
                }

                let mut host = "".to_string();
                if let Some(dns) = instance.public_dns_name() {
                    if !dns.is_empty() {
                        host = dns.into();
                    }
                }

                tracing::info!(
                    "{}. {:?}, type = {}, state = {:?}, {:?}",
                    i + 1,
                    name,
                    instance.instance_type.as_ref().unwrap(),
                    instance.state().unwrap().name().unwrap(),
                    host,
                );
            }
        }
        Commands::Delete { wait } => {
            if let Ok(chosen) =
                multi_select_instances(&ec2, "Choose the instance(s):", vec![]).await
            {
                let instance_ids = ids_to_str(chosen);
                if instance_ids.is_empty() {
                    tracing::warn!("Nothing is selected. Use [space] to select option.");
                } else {
                    ec2.delete_instances(&instance_ids, wait).await?;
                }
            }
        }
        Commands::Start => {
            if let Ok(chosen) = multi_select_instances(
                &ec2,
                "Choose the instance(s):",
                vec![InstanceStateName::Stopped],
            )
            .await
            {
                let instance_ids = ids_to_str(chosen);
                if instance_ids.is_empty() {
                    tracing::warn!("Nothing is selected. Use [space] to select option.");
                } else {
                    ec2.start_instances(&instance_ids).await?;
                }
            }
        }
        Commands::Stop { wait } => {
            if let Ok(chosen) = multi_select_instances(
                &ec2,
                "Choose the instance(s):",
                vec![InstanceStateName::Running],
            )
            .await
            {
                let instance_ids = ids_to_str(chosen);
                if instance_ids.is_empty() {
                    tracing::warn!("Nothing is selected. Use [space] to select option.");
                } else {
                    ec2.stop_instances(&instance_ids, wait).await?;
                }
            }
        }
        Commands::Upload { src, dst, user } => {
            if let Ok(chosen) = select_instance(
                &ec2,
                "Choose running instance to upload files to:",
                vec![InstanceStateName::Running],
            )
            .await
            {
                tracing::info!("Chosen instance: {} = {}", chosen.name, chosen.instance_id);
                // Refresh inbound IP.
                ec2.get_ssh_security_group().await?;
                let session =
                    Session::connect(&user, chosen.public_dns_name.unwrap(), ssh_path).await?;
                session.upload(src, dst).await?;
            } else {
                tracing::warn!("No active running instances to upload to.");
            }
        }
        Commands::Run { command, user } => {
            if command.is_empty() {
                tracing::warn!("Please enter a command to run.");
                return Ok(());
            }

            let chosen = select_instance(
                &ec2,
                "Choose running instance to execute remote command:",
                vec![InstanceStateName::Running],
            )
            .await
            .unwrap();
            tracing::info!(
                "Chosen instance: name = {}, instance_id = {}",
                chosen.name,
                chosen.instance_id
            );

            // Refresh inbound IP.
            ec2.get_ssh_security_group().await?;

            let mut session =
                Session::connect(&user, chosen.public_dns_name.unwrap(), ssh_path).await?;
            let _raw_term = std::io::stdout().into_raw_mode()?;
            // TODO: On centos, nothing is printed to stdout (message is received on SDK client).
            session
                .exec(
                    &command
                        .into_iter()
                        // arguments are escaped manually since the SSH protocol doesn't support quoting
                        .map(|cmd_part| shell_escape::escape(cmd_part.into()))
                        .collect::<Vec<_>>()
                        .join(" "),
                )
                .await?;
            session.close().await?;
        }
        Commands::Shell { user } => {
            let chosen = select_instance(
                &ec2,
                "Choose running instance to ssh:",
                vec![InstanceStateName::Running],
            )
            .await;

            if let Ok(chosen) = chosen {
                tracing::info!(
                    "Chosen instance: name = {}, instance_id = {}",
                    chosen.name,
                    chosen.instance_id
                );

                // Refresh inbound IP.
                ec2.get_ssh_security_group().await?;

                let mut session =
                    Session::connect(&user, chosen.public_dns_name.unwrap(), ssh_path).await?;
                let _raw_term = std::io::stdout().into_raw_mode()?;
                session
                    .exec(
                        &vec!["bash"]
                            .into_iter()
                            .map(|cmd_part| shell_escape::escape(cmd_part.into()))
                            .collect::<Vec<_>>()
                            .join(" "),
                    )
                    .await?;
                session.close().await?;
            } else {
                tracing::warn!("There are no active instances to SSH into.");
            }
        }
        Commands::Obliterate => {
            let yes = Text::new("Do you want to obliterate all resources [y/n]?:").prompt()?;
            if !(yes == "y" || yes == "Y") {
                tracing::warn!("Aborting obliterate.");
                return Ok(());
            }

            // Passing empty vec means all non-terminated instances are returned.
            let instances = ec2.describe_instance(vec![]).await?;
            let select_all = instances.into_iter().map(|i| i.into()).collect();
            let instance_ids = ids_to_str(select_all);

            let grp = ec2.describe_security_group(SSH_SECURITY_GROUP).await?;
            let grp_id = grp.as_ref().unwrap().group_id().unwrap();

            let key_pairs = ec2.list_key_pair(SSH_KEY_NAME).await?;
            let key_pair_ids: Vec<_> = key_pairs.iter().map(|k| k.key_pair_id().unwrap()).collect();

            tracing::info!("instance_ids = {:?}", instance_ids);
            tracing::info!("grp_id = {:?}", grp_id);
            tracing::info!("key pairs = {:?}", key_pair_ids);

            ec2.delete_instances(&instance_ids, true).await?;
            ec2.delete_security_group(grp_id).await?;
            for id in key_pair_ids {
                ec2.delete_key_pair(id).await?;
            }

            // Remove SSH key. PK is useless when key pair is deleted.
            std::fs::remove_file(&ssh_path)
                .with_context(|| format!("Failed to remove pk file at {ssh_path}."))?;
        }
    };

    Ok(())
}
