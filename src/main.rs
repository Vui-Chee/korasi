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

        /// Destination folder path on remote instance to upload files to.
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
    /// TODO: Allow client to send commands (read stdin) via SSH channel.
    /// eg. impt for sudo prompts
    ///
    /// TODO: enable blocking commands to function from local.
    /// eg. open a remote file using `vi`
    ///
    /// TODO: run cmd from target directory.
    Run {
        #[arg(allow_hyphen_values = true, num_args = 1..)]
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
                let channel = session.channel_open_session().await.unwrap();
                channel.request_subsystem(true, "sftp").await.unwrap();

                let sftp = SftpSession::new(channel.into_stream()).await.unwrap();

                let src_path = std::fs::canonicalize(src.unwrap_or(".".into()))?;
                let mut components = src_path.components();
                let root_folder = components
                    .next_back()
                    .unwrap()
                    .as_os_str()
                    .to_str()
                    .unwrap_or(".");
                let prefix = PathBuf::from(components.as_path());
                let dst_folder =
                    PathBuf::from(dst.as_ref().unwrap_or(&".".into())).join(root_folder);

                // Only create root folder, skip if any error occurred.
                let create_root_dir = sftp
                    .create_dir(dst_folder.to_str().unwrap().to_owned())
                    .await;

                tracing::warn!("create_root_dir = {:?}", create_root_dir);
                tracing::info!("root = {:?}", root_folder);
                tracing::info!("remote dst = {:?}", dst_folder);
                tracing::info!("prefix = {:?}", prefix);

                let dst_exists = sftp
                    .try_exists(dst_folder.to_str().unwrap().to_owned())
                    .await?;
                if !dst_exists {
                    tracing::warn!("Remote dst folder does not exist. Aborting upload.");
                    return Ok(());
                }

                // The .gitignore at src_path will be respected.
                for result in Walk::new(src_path) {
                    match result {
                        Ok(entry) => {
                            let data = entry.metadata();

                            let mut abs_local_pth = entry
                                .path()
                                .to_str()
                                .unwrap()
                                .strip_prefix(prefix.to_str().unwrap_or(""))
                                .unwrap()
                                .chars();
                            abs_local_pth.next();
                            let rel_pth = abs_local_pth.as_str();
                            if let Some(ref _dst) = dst {
                                // rel_pth = &format!("");
                            }
                            tracing::info!("dst = {:?}, rel_pth = {:?}", dst, rel_pth);

                            // TODO: Attach remote dst folder path to rel_pth.

                            // (dst + root) + pth
                            if let Ok(data) = data {
                                if data.is_dir() {
                                    // WRONG: abs path to where directory will be created.
                                    let _ = sftp.create_dir(rel_pth).await;
                                } else {
                                }
                            }
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
