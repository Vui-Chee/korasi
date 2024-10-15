pub mod create;
pub mod ec2;
pub mod ssh;
pub mod util;

use aws_config::{
    self, meta::region::RegionProviderChain, timeout::TimeoutConfig, BehaviorVersion,
};
use aws_types::{region::Region, SdkConfig as AwsSdkConfig};
use tokio::time::Duration;

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
