//! IO Utilities wrapper to allow automock for requests and user input prompts.

use ignore::Error;
use std::{
    fmt::{self, Display},
    io::Write,
    path::{Path, PathBuf},
};

use aws_sdk_ec2::types::{
    Image, Instance, InstanceStateName, InstanceType, KeyFormat, KeyPairInfo, KeyType,
};
use ignore::Walk;
use inquire::{InquireError, MultiSelect, Select};

use crate::ec2::SSH_KEY_NAME;
use crate::ec2::{EC2Error, EC2Impl as EC2};

#[derive(Default)]
pub struct UtilImpl;

impl UtilImpl {
    /// Utility to perform a GET request and return the body as UTF-8, or an appropriate EC2Error.
    pub async fn do_get(url: &str) -> Result<String, EC2Error> {
        reqwest::get(url)
            .await
            .map_err(|e| EC2Error::new(format!("Could not request ip from {url}: {e:?}")))?
            .error_for_status()
            .map_err(|e| EC2Error::new(format!("Failure status from {url}: {e:?}")))?
            .text_with_charset("utf-8")
            .await
            .map_err(|e| EC2Error::new(format!("Failed to read response from {url}: {e:?}")))
    }

    pub fn write_secure(path: &PathBuf, material: String, mode: u32) -> Result<(), EC2Error> {
        let mut file = open_file_with_perm(path, mode)?;
        file.write(material.as_bytes())
            .map_err(|e| EC2Error::new(format!("Failed to write to {path:?} ({e:?})")))?;
        Ok(())
    }

    pub async fn create_or_get_keypair(
        ec2: &EC2,
        save_location: String,
    ) -> Result<Option<KeyPairInfo>, EC2Error> {
        match ec2
            .create_key_pair(SSH_KEY_NAME, KeyType::Ed25519, KeyFormat::Pem)
            .await
        {
            Ok((info, material)) => {
                tracing::info!("Saving PK to file...");

                // Save private key.
                UtilImpl::write_secure(&save_location.into(), material, 0o400)?;

                Ok(Some(info))
            }
            Err(err) => {
                // NOTE: This assumes user already saved the private key locally.
                tracing::warn!("No key pair is created. Err = {}", err);
                let output = ec2.list_key_pair(SSH_KEY_NAME).await?;
                if !output.is_empty() {
                    tracing::info!(
                        "Reuse existing key pair: {:?}",
                        output[0].key_name.as_ref().unwrap()
                    );
                    Ok(Some(output[0].clone()))
                } else {
                    tracing::error!("No instance is created since no existing key pair is found.");
                    Ok(None)
                }
            }
        }
    }
}

#[cfg(unix)]
fn open_file_with_perm(path: &PathBuf, mode: u32) -> Result<std::fs::File, EC2Error> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .mode(mode)
        .write(true)
        .create(true)
        .open(path)
        .map_err(|e| EC2Error::new(format!("Failed to create {path:?} ({e:?})")))
}

#[cfg(not(unix))]
fn open_file(path: &PathBuf) -> Result<File, EC2Error> {
    fs::File::create(path.clone())
        .map_err(|e| EC2Error::new(format!("Failed to create {path:?} ({e:?})")))?
}

/// Image doesn't impl Display, which is necessary for inquire to use it in a Select.
/// This wraps Image and provides a Display impl.
#[derive(PartialEq, Debug)]
pub struct ScenarioImage(pub Image);
impl From<Image> for ScenarioImage {
    fn from(value: Image) -> Self {
        ScenarioImage(value)
    }
}

impl Display for ScenarioImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {}",
            self.0.name().unwrap_or("(unknown)"),
            self.0.description().unwrap_or("unknown")
        )
    }
}

#[derive(Debug, Default, Clone)]
pub struct SelectOption {
    pub name: String,
    pub instance_id: String,
    pub public_dns_name: Option<String>,
    state: Option<InstanceStateName>,
    instance_type: Option<InstanceType>,
}

impl fmt::Display for SelectOption {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let status = self.state.as_ref().unwrap().clone();
        write!(
            f,
            "name = {}, type = {}, instance_id = {}, status = {}",
            self.name,
            self.instance_type.as_ref().unwrap(),
            self.instance_id,
            status
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

        opt.instance_type = value.instance_type().cloned();
        for t in value.tags() {
            if t.key() == Some("Name") {
                opt.name = t.value().unwrap().to_owned();
            }
        }

        opt
    }
}

/// Express list of instance ids as a comma separated string.
pub fn ids_to_str(ids: Vec<SelectOption>) -> String {
    ids.iter()
        .map(|i| i.instance_id.to_owned())
        .collect::<Vec<_>>()
        .join(",")
}

pub async fn multi_select_instances(
    ec2: &EC2,
    prompt: &str,
    statuses: Vec<InstanceStateName>,
) -> Result<Vec<SelectOption>, InquireError> {
    // Get all instances tagged by this tool.
    let instances = ec2.describe_instance(statuses).await.unwrap();
    let options: Vec<SelectOption> = instances.into_iter().map(|i| i.into()).collect();

    if options.len() == 1 {
        return Ok(vec![options[0].to_owned()]);
    }
    MultiSelect::new(prompt, options)
        .with_vim_mode(true)
        .prompt()
}

pub async fn select_instance(
    ec2: &EC2,
    prompt: &str,
    statuses: Vec<InstanceStateName>,
) -> Result<SelectOption, InquireError> {
    let instances = ec2.describe_instance(statuses).await.unwrap();
    let options: Vec<SelectOption> = instances.into_iter().map(|i| i.into()).collect();

    if options.len() == 1 {
        return Ok(options[0].to_owned());
    }
    Select::new(prompt, options).with_vim_mode(true).prompt()
}

pub fn calc_prefix(pth: PathBuf) -> std::io::Result<PathBuf> {
    Ok(pth.parent().unwrap_or(Path::new("")).to_path_buf())
}

pub fn biject_paths<'a>(
    src_path: &str,
    prefix: &'a str,
    dst_folder: &'a str,
) -> Vec<Result<(PathBuf, PathBuf, bool), Error>> {
    Walk::new(src_path)
        .map(move |result| match result {
            Ok(entry) => {
                let is_dir = match entry.metadata() {
                    Ok(ent) => ent.is_dir(),
                    _ => false,
                };
                let local_pth = entry.path().to_path_buf();
                let mut rel_pth = entry
                    .path()
                    .to_str()
                    .unwrap()
                    .strip_prefix(prefix)
                    .unwrap()
                    .chars();
                rel_pth.next();
                let transformed = PathBuf::from(dst_folder).join(rel_pth.as_str());

                tracing::info!("uploaded path = {:?}", transformed);

                Ok((local_pth, transformed, is_dir))
            }
            Err(err) => Err(err),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{
        fs::remove_file,
        path::{Path, PathBuf},
    };

    use crate::util::biject_paths;

    use super::{calc_prefix, open_file_with_perm};

    #[test]
    fn open_readonly_file() {
        let pk_file = "pk.pem";

        assert!(
            !Path::new(pk_file).exists(),
            "Test pk file should not exist before test."
        );
        let _ = open_file_with_perm(&pk_file.into(), 0o400);
        let meta = std::fs::metadata(pk_file).unwrap();
        assert!(
            meta.permissions().readonly(),
            "ssh PK file should be readonly."
        );
        let _ = remove_file(pk_file);
    }

    #[test]
    fn calc_src_prefix() {
        let _ = std::fs::remove_dir("../outside-cwd");

        let cwd = std::env::current_dir().unwrap();
        std::fs::create_dir("../outside-cwd").unwrap();

        let cases = [
            ("/", PathBuf::from("")),
            ("README.md", cwd.clone()),
            ("src/main.rs", cwd.join("src")),
            ("../outside-cwd", cwd.parent().unwrap().to_path_buf()),
        ];

        for (input, expected) in cases {
            println!("input = {input}");
            let canon_pth = std::fs::canonicalize(input).unwrap();
            let got = calc_prefix(canon_pth);
            assert!(
                got.is_ok(),
                "Failed to canonicalize path = {}, Err = {}",
                input,
                got.unwrap_err()
            );
            pretty_assertions::assert_eq!(got.unwrap(), expected);
        }

        std::fs::remove_dir("../outside-cwd").unwrap();
    }

    #[test]
    fn calc_remote_paths() {
        let cwd = std::env::current_dir().unwrap();

        let cases = [
            (
                // Paths are unchanged
                cwd.as_path().to_str().unwrap(),
                "",
                "/home/foobar",
            ),
            (
                // Paths prefixes are replaced
                cwd.as_path().to_str().unwrap(),
                cwd.parent().unwrap().to_str().unwrap(),
                "/home/foobar",
            ),
        ];

        for (x, y, z) in cases {
            for result in biject_paths(x, y, z) {
                match result {
                    Ok(entry) => {
                        println!("entry = {:?}", entry);
                    }
                    Err(err) => {
                        println!("err = {}", err);
                    }
                }
            }
            println!();
        }
    }
}
