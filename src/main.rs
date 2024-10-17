use std::fmt;
use std::sync::Arc;

use aws_sdk_ec2::types::{Instance, InstanceStateName, InstanceType};
use clap::{Parser, Subcommand};
use infra::load_config;
use infra::ssh::{exec, load_secret_key};
use infra::{create::CreateCommand, ssh::ClientSSH};
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

    /// Path to SSH private key.
    #[structopt(short, long, default_value = "ec2-ssh-key.pem")]
    ssh_key: String,

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

    /// Upload local file(s) or directory to remote target instance.
    ///
    /// Uses SFTP that rides on top of SSH to transfer files.
    Upload {
        /// Local relative/absolute path to file(s) or directory.
        ///
        /// Relative takes reference from current working directory.
        #[arg(long, short)]
        src: String,

        /// Destination file path on remote instance to upload files to.
        #[arg(long, short)]
        dest: String,
    },

    /// Executes a given command on remote instance.
    Run {
        #[arg(num_args = 1..)]
        command: Vec<String>,

        /// Set's the current working directory to execute command in.
        /// Default path is $HOME.
        ///
        /// Think of it as a remote `cd`.
        #[arg(long, short, default_value = "")]
        path: String,
    },
}

#[tokio::main]
async fn main() -> Result<(), EC2Error> {
    let Opt {
        profile,
        debug,
        region,
        commands,
        ssh_key,
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
                tracing::info!(
                    "{}. instance ({}) = {:?}, state = {:?}",
                    i + 1,
                    name,
                    instance.instance_id().unwrap(),
                    instance.state().unwrap().name().unwrap(),
                );
            }
        }
        Commands::Upload { .. } => {}
        Commands::Run { command, .. } => {
            if command.is_empty() {
                tracing::warn!("Please enter a command to run.");
                return Ok(());
            }

            let chosen = select_instance(&ec2, "Choose the instance to execute remote command:")
                .await
                .unwrap();
            tracing::info!("Chosen instance: {:?}", chosen);

            let config = russh::client::Config::default();
            let mut session = russh::client::connect(
                Arc::new(config),
                (chosen.public_dns_name.unwrap(), 22),
                ClientSSH {},
            )
            .await
            .expect("Failed to establish SSH connection with remote instance.");

            let key_pair = load_secret_key(ssh_key, None).unwrap();

            if session
                .authenticate_publickey("ubuntu", Arc::new(key_pair))
                .await
                .unwrap()
            {
                tracing::info!("Successful auth");

                exec(
                    session,
                    &command
                        .into_iter()
                        // arguments are escaped manually since the SSH protocol doesn't support quoting
                        .map(|cmd_part| shell_escape::escape(cmd_part.into()))
                        .collect::<Vec<_>>()
                        .join(" "),
                )
                .await
                .unwrap();
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
                        // TODO: can only start instances that are stopped.
                        ec2.start_instances(&instance_ids).await?;
                    } else if let Commands::Stop { wait } = remaining {
                        // TODO: Can only stop instances that are running.
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
fn ids_to_str(ids: Vec<SelectOption>) -> String {
    ids.iter()
        .map(|i| i.instance_id.to_owned())
        .collect::<Vec<_>>()
        .join(",")
}

async fn multi_select_instances(
    ec2: &EC2,
    prompt: &str,
) -> Result<Vec<SelectOption>, InquireError> {
    // Get all instances tagged by this tool.
    let instances = ec2.describe_instance(vec![]).await.unwrap();
    let options: Vec<SelectOption> = instances.into_iter().map(|i| i.into()).collect();

    MultiSelect::new(prompt, options)
        .with_vim_mode(true)
        .prompt()
}

async fn select_instance(ec2: &EC2, prompt: &str) -> Result<SelectOption, InquireError> {
    let instances = ec2
        .describe_instance(vec![InstanceStateName::Running])
        .await
        .unwrap();
    let options: Vec<SelectOption> = instances.into_iter().map(|i| i.into()).collect();

    if options.len() == 1 {
        return Ok(options[0].to_owned());
    }
    Select::new(prompt, options).with_vim_mode(true).prompt()
}

#[derive(Debug, Default, Clone)]
struct SelectOption {
    name: String,
    instance_id: String,
    public_dns_name: Option<String>,
    state: Option<InstanceStateName>,
}

impl fmt::Display for SelectOption {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let status = self.state.as_ref().unwrap().clone();
        write!(
            f,
            "name = {}, instance_id = {}, status = {}",
            self.name, self.instance_id, status
        )
    }
}

impl From<Instance> for SelectOption {
    fn from(value: Instance) -> Self {
        let mut opt = SelectOption {
            state: value.state().unwrap().name().cloned(),
            instance_id: value.instance_id().unwrap().to_string(),
            public_dns_name: value.public_dns_name().map(str::to_string),
            ..SelectOption::default()
        };

        for t in value.tags() {
            if t.key() == Some("Name") {
                opt.name = t.value().unwrap().to_owned();
            }
        }

        opt
    }
}
