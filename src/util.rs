//! IO Utilities wrapper to allow automock for requests and user input prompts.

use std::{
    fmt::{self, Display},
    io::Write,
    path::PathBuf,
};

use aws_sdk_ec2::types::{Image, Instance, InstanceStateName};
use inquire::{InquireError, MultiSelect, Select};

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

    pub fn write_secure(
        key_name: &str,
        path: &PathBuf,
        material: String,
        mode: u32,
    ) -> Result<(), EC2Error> {
        let mut file = open_file_with_perm(path, mode)?;
        file.write(material.as_bytes()).map_err(|e| {
            EC2Error::new(format!("Failed to write {key_name} to {path:?} ({e:?})"))
        })?;
        Ok(())
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
    name: String,
    instance_id: String,
    pub public_dns_name: Option<String>,
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
) -> Result<Vec<SelectOption>, InquireError> {
    // Get all instances tagged by this tool.
    let instances = ec2.describe_instance(vec![]).await.unwrap();
    let options: Vec<SelectOption> = instances.into_iter().map(|i| i.into()).collect();

    MultiSelect::new(prompt, options)
        .with_vim_mode(true)
        .prompt()
}

pub async fn select_instance(ec2: &EC2, prompt: &str) -> Result<SelectOption, InquireError> {
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

#[cfg(test)]
mod ec2 {
    use std::{fs::remove_file, path::Path};

    use super::open_file_with_perm;

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
        assert!(
            remove_file(pk_file).is_ok(),
            "Failed to remove test pk file."
        );
    }
}
