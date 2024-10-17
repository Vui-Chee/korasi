use std::{io::Read, path::Path, sync::Arc};

use async_trait::async_trait;
use russh::{
    client::{self, Handle},
    keys::{decode_secret_key, key},
    ChannelId, ChannelMsg,
};
use tokio::io::AsyncWriteExt;

pub struct ClientSSH;

#[async_trait]
impl client::Handler for ClientSSH {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &key::PublicKey,
    ) -> Result<bool, Self::Error> {
        tracing::info!("check_server_key: {:?}", server_public_key);
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

pub async fn connect(
    public_dns_name: String,
    ssh_key: String,
) -> anyhow::Result<Handle<ClientSSH>> {
    let config = russh::client::Config::default();
    let mut session = russh::client::connect(Arc::new(config), (public_dns_name, 22), ClientSSH {})
        .await
        .expect("Failed to establish SSH connection with remote instance.");
    let key_pair = load_secret_key(ssh_key, None).unwrap();

    session
        // TODO: Do not hardcode user
        .authenticate_publickey("ubuntu", Arc::new(key_pair))
        .await?;

    Ok(session)
}

pub async fn exec(session: Handle<ClientSSH>, command: &str) -> anyhow::Result<u32> {
    let mut channel = session.channel_open_session().await?;
    channel.exec(true, command).await?;

    let mut code = None;
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();

    loop {
        // There's an event available on the session channel
        let Some(msg) = channel.wait().await else {
            break;
        };
        match msg {
            // Write data to the terminal
            ChannelMsg::Data { ref data } => {
                stdout.write_all(data).await?;
                stdout.flush().await?;
            }
            // The command has returned an exit code
            ChannelMsg::ExitStatus { exit_status } => {
                code = Some(exit_status);
                // cannot leave the loop immediately, there might still be more data to receive
            }
            ChannelMsg::ExtendedData { ref data, ext: _ } => {
                stderr.write_all(data).await?;
                stderr.flush().await?;
            }
            _ => {}
        }
    }
    Ok(code.expect("program did not exit cleanly"))
}
