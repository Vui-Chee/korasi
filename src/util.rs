//! IO Utilities wrapper to allow automock for requests and user input prompts.

use std::{fmt::Display, io::Write, path::PathBuf};

use aws_sdk_ec2::types::Image;

use crate::ec2::EC2Error;

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
