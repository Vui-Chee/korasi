use std::{fs::File, io::Read, path::Path, sync::Arc};

use async_trait::async_trait;
use russh::{
    client::{self, Msg},
    keys::{decode_secret_key, key},
    Channel, ChannelId, ChannelMsg, Disconnect,
};
use russh_sftp::{client::SftpSession, protocol::OpenFlags};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::util::{biject_paths, calc_prefix};

pub const SSH_PORT: u16 = 22;

pub struct ClientSSH;

#[async_trait]
impl client::Handler for ClientSSH {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &key::PublicKey,
    ) -> Result<bool, Self::Error> {
        tracing::debug!("check_server_key: {:?}", server_public_key);
        Ok(true)
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        tracing::debug!("data on channel {:?}: {}", channel, data.len());
        Ok(())
    }
}

pub struct Session {
    session: client::Handle<ClientSSH>,
}

impl Session {
    /// Returns reusable remote channel that can used as a SSH/SFTP tunnel.
    ///
    /// This prevents direct access to private session.
    pub async fn channel_open_session(&self) -> Result<Channel<Msg>, russh::Error> {
        self.session.channel_open_session().await
    }

    /// Load a secret key, deciphering it with the supplied password if necessary.
    pub fn load_secret_key<P: AsRef<Path>>(
        secret_: P,
        password: Option<&str>,
    ) -> Result<key::KeyPair, anyhow::Error> {
        let mut secret_file = std::fs::File::open(secret_)?;
        let mut secret = String::new();
        secret_file.read_to_string(&mut secret)?;
        Ok(decode_secret_key(&secret, password)?)
    }

    /// Connect to remote instance via SSH.
    ///
    /// The public DNS name is the emphemeral host address generated when
    /// an EC2 instance starts.
    pub async fn connect(
        user: &str,
        public_dns_name: String,
        ssh_key: String,
    ) -> anyhow::Result<Self> {
        let config = russh::client::Config {
            inactivity_timeout: Some(std::time::Duration::from_secs(1200)), // 20 min.
            ..<_>::default()
        };
        let mut session =
            russh::client::connect(Arc::new(config), (public_dns_name, SSH_PORT), ClientSSH {})
                .await
                .expect("Failed to establish SSH connection with remote instance.");
        let key_pair = Self::load_secret_key(ssh_key, None).unwrap();

        session
            .authenticate_publickey(user, Arc::new(key_pair))
            .await?;

        Ok(Self { session })
    }

    /// Executes a remote command using SSH.
    pub async fn exec(&self, command: &str) -> anyhow::Result<u32> {
        let mut channel = self.channel_open_session().await?;

        // No terminal resizing after the connection is established.
        let (w, h) = termion::terminal_size()?;
        // Request an interactive PTY from the server.
        channel
            .request_pty(
                false,
                &std::env::var("TERM").unwrap_or("xterm".into()),
                w as u32,
                h as u32,
                0,
                0,
                &[], // ideally you want to pass the actual terminal modes here
            )
            .await?;

        channel.exec(true, command).await?;

        let mut stdin = tokio_fd::AsyncFd::try_from(0)?;
        let mut stdout = tokio_fd::AsyncFd::try_from(1)?;
        let mut stderr = tokio_fd::AsyncFd::try_from(2)?;

        let code;
        let mut buf = vec![0; 1024];
        let mut stdin_closed = false;

        loop {
            tokio::select! {
                r = stdin.read(&mut buf), if !stdin_closed => {
                    match r {
                        Ok(0) => {
                            stdin_closed = true;
                            channel.eof().await?;
                        },
                        // Send it to the server
                        Ok(n) => channel.data(&buf[..n]).await?,
                        Err(e) => return Err(e.into()),
                    };
                },
                Some(msg) = channel.wait() => {
                    match msg {
                        // Write data to the terminal
                        ChannelMsg::Data { ref data } => {
                            stdout.write_all(data).await?;
                            stdout.flush().await?;
                        }
                        ChannelMsg::ExitStatus { exit_status } => {
                            code = Some(exit_status);
                            if !stdin_closed {
                                channel.eof().await?;
                            }
                            break;
                        }
                        // Get std error from remote command.
                        ChannelMsg::ExtendedData { ref data, ext: _ } => {
                            stderr.write_all(data).await?;
                            stderr.flush().await?;
                        }
                        _ => {}
                    }
                },
            }
        }

        Ok(code.expect("program did not exit cleanly"))
    }

    async fn open_sftp_session(&self) -> Result<SftpSession, russh_sftp::client::error::Error> {
        let channel = self.session.channel_open_session().await.unwrap();
        channel.request_subsystem(true, "sftp").await.unwrap();

        SftpSession::new(channel.into_stream()).await
    }

    /// Upload files within `src` to `dst` directory using SFTP.
    /// If `dst` is not specified, files will uploaded to $HOME/{cwd}.
    /// The {cwd} folder will be created by default in this use case.
    ///
    /// Panics if dst is not a directory.
    pub async fn upload(&self, src: Option<String>, dst: Option<String>) -> anyhow::Result<()> {
        let src_path = match std::fs::canonicalize(src.unwrap_or(".".into())) {
            Ok(pth) => pth,
            // Bail early if the src path is fked.
            Err(err) => anyhow::bail!("Failed to canonicalize src = {err}"),
        };

        let sftp = self.open_sftp_session().await?;

        if dst.is_some() {
            match sftp.metadata(dst.as_ref().unwrap_or(&".".into())).await {
                Ok(attr) => {
                    if !attr.is_dir() {
                        anyhow::bail!("Dst must be a dir!");
                    }
                }
                Err(err) => {
                    tracing::error!("Error remote metadata = {err}");
                    return Ok(());
                }
            }
        }

        let prefix = calc_prefix(src_path.clone())?;
        let dst_abs_path = sftp
            .canonicalize(&dst.unwrap_or(".".into()))
            .await
            .expect("Failed to canonicalize remote dst.");

        // The .gitignore at src_path will be respected.
        for result in biject_paths(
            src_path.to_str().unwrap(),
            prefix.to_str().unwrap_or(""),
            &dst_abs_path,
        ) {
            match result {
                Ok((local_pth, combined, is_dir)) => {
                    if is_dir {
                        let _ = sftp.create_dir(combined.to_str().unwrap().to_owned()).await;
                    } else {
                        let open_remote_file = sftp
                            .open_with_flags(
                                combined.to_str().unwrap(),
                                OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
                            )
                            .await;
                        if open_remote_file.is_err() {
                            tracing::warn!("Failed to open file = {:?}", combined,);
                        }

                        // Overwrite remote file contents with local file contents.
                        if let Ok(mut remote_file) = open_remote_file {
                            let mut local_file = File::open(local_pth).unwrap();
                            let mut buffer = Vec::new();
                            local_file.read_to_end(&mut buffer).unwrap();
                            remote_file.write_all(buffer.as_slice()).await.unwrap();
                            let _ = remote_file.sync_all().await;
                            remote_file.shutdown().await.unwrap();
                        }
                    }
                }
                Err(err) => tracing::error!("ERROR: {}", err),
            }
        }

        sftp.close().await?;

        Ok(())
    }

    /// Closes SSH session.
    pub async fn close(&mut self) -> anyhow::Result<()> {
        self.session
            .disconnect(Disconnect::ByApplication, "", "English")
            .await?;
        Ok(())
    }
}
