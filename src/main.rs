use std::path::PathBuf;

use aws_sdk_ec2::types::{InstanceStateName, InstanceType};
use clap::{Parser, Subcommand};
use ignore::Walk;
use inquire::Select;
use russh_sftp::client::SftpSession;

use infra::create::CreateCommand;
use infra::ec2::EC2Impl as EC2;
use infra::load_config;
use infra::ssh::{connect, exec};
use infra::util::{ids_to_str, multi_select_instances, select_instance};

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
        /// If no `src` is specified, then files within with current
        /// working directory will be uploaded to $HOME of remote.
        #[arg(index = 1)]
        src: Option<String>,

        /// Destination file path on remote instance to upload files to.
        ///
        /// If no dst is specified, files will be uploaded the $HOME
        /// directory of remote.
        #[arg(index = 2)]
        dst: Option<String>,
    },

    /// Executes a given command on remote instance.
    ///
    /// Only run commands that are non-blocking. Commands like
    /// opening `vi` does not working at the moment.
    ///
    /// TODO: enable blocking commands to function from local.
    /// eg. open a remote file using `vi`
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
async fn main() -> anyhow::Result<()> {
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
        Commands::Delete { wait } => {
            if let Ok(chosen) =
                multi_select_instances(&ec2, "Choose the instance(s):", vec![]).await
            {
                let instance_ids = ids_to_str(chosen);
                ec2.delete_instances(&instance_ids, wait).await?;
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
                ec2.start_instances(&instance_ids).await?;
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
                ec2.stop_instances(&instance_ids, wait).await?;
            }
        }
        Commands::Upload { src, dst } => {
            tracing::info!("{:?}, {:?}", src, dst);

            let chosen = select_instance(
                &ec2,
                "Choose running instance to upload files to:",
                vec![InstanceStateName::Running],
            )
            .await
            .unwrap();
            tracing::info!("Chosen instance: {:?}", chosen);

            let session = connect(chosen.public_dns_name.unwrap(), ssh_key).await;

            if let Ok(session) = session {
                tracing::info!("Successful auth");

                let channel = session.channel_open_session().await.unwrap();
                channel.request_subsystem(true, "sftp").await.unwrap();
                let sftp = SftpSession::new(channel.into_stream()).await.unwrap();
                tracing::info!(
                    "current remote path: {:?}",
                    sftp.canonicalize(".").await.unwrap()
                );

                let p = std::fs::canonicalize(".")?;
                let mut components = p.components();
                let root = components.next_back();
                let prefix = PathBuf::from(components.as_path());

                tracing::info!("root = {:?}", root);
                tracing::info!("prefix = {:?}", prefix);

                for result in Walk::new("./") {
                    // Each item yielded by the iterator is either a directory entry or an
                    // error, so either print the path or the error.
                    match result {
                        Ok(entry) => {
                            let data = entry.metadata();
                            tracing::info!("{}", entry.path().display());
                        }
                        Err(err) => tracing::error!("ERROR: {}", err),
                    }
                }
            }
        }
        Commands::Run { command, .. } => {
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
            tracing::info!("Chosen instance: {:?}", chosen);

            let session = connect(chosen.public_dns_name.unwrap(), ssh_key).await;

            if let Ok(session) = session {
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
    };

    Ok(())
}
