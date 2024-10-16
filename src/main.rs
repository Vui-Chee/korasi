use aws_sdk_ec2::types::InstanceType;
use clap::{Parser, Subcommand};
use infra::create::CreateCommand;
use infra::load_config;
use inquire::{InquireError, MultiSelect, Select};

use infra::ec2::{EC2Error, EC2Impl as EC2};

#[derive(Debug, Parser)]
#[command(arg_required_else_help = true)]
struct Opt {
    /// AWS credentials profile to use (set in ~/.aws/credentials).
    #[structopt(short, long, default_value = "default")]
    profile: String,

    /// Select region where ec2 instance is located.
    #[structopt(short, long, default_value = "ap-southeast-1")]
    region: String,

    /// Enable to show logs.
    #[structopt(short, default_value_t = false)]
    debug: bool,

    /// Specify path to launch script.
    #[structopt(long, default_value = "start_up.sh")]
    setup: String,

    #[command(subcommand)]
    commands: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Create new instance, and print out SSH link.
    ///
    /// If not machine_type is specified, allow user to
    /// choose machine_type from list of options.
    Create { ami_id: String },

    /// List all instances created by this tool, which is under
    /// the same tag.
    List,

    /// Delete 1 or more instances, where all options are displayed
    /// using a multi-select input.
    Delete {
        #[arg(long, short, default_value_t = true)]
        wait: bool,
    },

    /// Start 1 or more instances.
    ///
    /// Starting a stopped instance without an EIP will
    /// result in a new IP being assigned.
    Start,

    /// Stop 1 or more instances.
    Stop {
        #[arg(long, short)]
        wait: bool,
    },
}

#[tokio::main]
async fn main() -> Result<(), EC2Error> {
    let Opt {
        profile,
        debug,
        region,
        commands,
        ..
    } = Opt::parse();

    if debug {
        tracing_subscriber::fmt().init();
    }

    let shared_config = load_config(Some(region), Some(profile), None).await;
    let client = aws_sdk_ec2::Client::new(&shared_config);
    let ec2 = EC2::new(client);

    match commands {
        Commands::Create { ami_id } => {
            let machine: InstanceType =
                Select::new("Select the machine type:", InstanceType::values().to_vec())
                    .prompt()
                    .unwrap()
                    .into();
            tracing::info!("Launching {machine} instance...");
            CreateCommand
                .launch(
                    &ec2,
                    machine,
                    ami_id,
                    "ec2-ssh-key".into(),
                    "start_up.sh".into(),
                )
                .await?;
        }
        Commands::List => {
            let res = ec2.describe_instance().await.unwrap();
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
                tracing::info!(
                    "{}. instance ({}) = {:?}, state = {:?}",
                    i + 1,
                    name,
                    instance.instance_id().unwrap(),
                    instance.state().unwrap().name().unwrap(),
                );
            }
        }
        remaining => {
            let chosen = multi_select_instances(&ec2, "Choose the instance(s):").await;

            if let Ok(chosen) = chosen {
                if chosen.is_empty() {
                    tracing::warn!("No instance was selected.");
                } else {
                    let instance_ids = ids_to_str(chosen);

                    if let Commands::Delete { wait } = remaining {
                        ec2.delete_instances(&instance_ids, wait).await?;
                    } else if let Commands::Start = remaining {
                        ec2.start_instances(&instance_ids).await?;
                    } else if let Commands::Stop { wait } = remaining {
                        ec2.stop_instances(&instance_ids, wait).await?;
                    }
                }
            } else {
                tracing::warn!("No active instances.");
            }
        }
    };

    Ok(())
}

/// Express list of instance ids as a comma separated string.
fn ids_to_str(ids: Vec<String>) -> String {
    ids.iter()
        .map(|x| x.split(":").collect::<Vec<_>>()[1])
        .collect::<Vec<_>>()
        .join(",")
}

async fn multi_select_instances(ec2: &EC2, prompt: &str) -> Result<Vec<String>, InquireError> {
    // Get all instances tagged by this tool.
    let instances = ec2.describe_instance().await.unwrap();

    let options: Vec<_> = instances
        .iter()
        .map(|i| {
            let mut name = "";
            let status = i.state().unwrap().name().unwrap();
            for t in i.tags() {
                if t.key() == Some("Name") {
                    name = t.value().unwrap();
                }
            }
            if name.is_empty() {
                name = "(empty)";
            }
            format!("{}:{}:{}", name, i.instance_id().unwrap(), status)
        })
        .collect();

    MultiSelect::new(prompt, options)
        .with_vim_mode(true)
        .prompt()
}
