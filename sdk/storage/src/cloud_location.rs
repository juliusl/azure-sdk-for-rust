use crate::{clients::ServiceType, StorageCredentials};
use once_cell::sync::Lazy;
use std::{
    convert::TryFrom,
    fs::File,
    io::{BufRead, BufReader},
    path::PathBuf,
};
use url::Url;

const AZURE_CLOUD: &str = "AzureCloud";
const AZURE_PUBLIC_CLOUD: &str = "AzurePublicCloud";
const AZURE_CHINA_CLOUD: &str = "AzureChinaCloud";
const AZURE_US_GOV: &str = "AzureUSGovernment";

/// The cloud with which you want to interact.
#[derive(Debug, Clone)]
pub enum CloudLocation {
    /// Azure public cloud
    Public {
        account: String,
        credentials: StorageCredentials,
    },
    /// Azure China cloud
    China {
        account: String,
        credentials: StorageCredentials,
    },
    /// Azure US Government
    USGov {
        account: String,
        credentials: StorageCredentials,
    },
    /// Use the well-known emulator
    Emulator { address: String, port: u16 },
    /// Auto-detect location based on `AZURE_CLOUD_NAME` variable or `$HOME/.azure/config`
    AutoDetect {
        account: String,
        credentials: StorageCredentials,
    },
    /// A custom base URL
    Custom {
        uri: String,
        credentials: StorageCredentials,
    },
}

impl CloudLocation {
    /// Returns a cloud location that will auto-detect the current cloud,
    ///
    /// This will auto detect first by checking the `AZURE_CLOUD_NAME` env variable, and then next
    /// by parsing the current user's `$HOME/.azure/config` file.
    ///
    /// If either of these methods fail, this will default to public azure.
    ///
    /// Cloud names can be listed with, `az cloud list --output table`. Current public values are:
    /// - AzureCloud - Public azure, in some cases may be AzurePublicCloud in env var form
    /// - AzureChinaCloud
    /// - AzureUSGovernment
    ///
    /// Excluded:
    /// - AzureGermanCloud - Shows up in the above command, but officially deprecated in 2021. Documented for posterity.
    ///
    pub fn auto_detect(
        account: impl Into<String>,
        credentials: StorageCredentials,
    ) -> CloudLocation {
        CloudLocation::AutoDetect {
            account: account.into(),
            credentials,
        }
    }

    /// the base URL for a given cloud location
    pub fn url(&self, service_type: ServiceType) -> azure_core::Result<Url> {
        let url = match self {
            CloudLocation::Public { account, .. } => {
                format!(
                    "https://{}.{}.core.windows.net",
                    account,
                    service_type.subdomain()
                )
            }
            CloudLocation::China { account, .. } => {
                format!(
                    "https://{}.{}.core.chinacloudapi.cn",
                    account,
                    service_type.subdomain()
                )
            }
            CloudLocation::USGov { account, .. } => {
                format!(
                    "https://{}.{}.core.usgovcloudapi.net",
                    account,
                    service_type.subdomain()
                )
            }
            CloudLocation::Custom { uri, .. } => uri.clone(),
            CloudLocation::Emulator { address, port } => {
                format!("http://{address}:{port}/{EMULATOR_ACCOUNT}")
            }
            CloudLocation::AutoDetect {
                account,
                credentials,
            } => {
                if let Some(name) = Self::find_cloud_name() {
                    // These names are from
                    // `az cloud list --output table`
                    return match name.as_str() {
                        // Seems like "AzurePublicCloud" is used in some environments
                        AZURE_CLOUD | AZURE_PUBLIC_CLOUD => CloudLocation::Public {
                            account: account.clone(),
                            credentials: credentials.clone(),
                        }
                        .url(service_type),
                        AZURE_US_GOV => CloudLocation::USGov {
                            account: account.clone(),
                            credentials: credentials.clone(),
                        }
                        .url(service_type),
                        AZURE_CHINA_CLOUD => CloudLocation::China {
                            account: account.clone(),
                            credentials: credentials.clone(),
                        }
                        .url(service_type),
                        _ => {
                            return Err(azure_core::Error::with_message(
                                azure_core::error::ErrorKind::Other,
                                || {
                                    format!(
                                        "Auto-detect encountered an invalid cloud name, allowed values are: {AZURE_CLOUD}, {AZURE_PUBLIC_CLOUD}, {AZURE_US_GOV}, {AZURE_CHINA_CLOUD}.",
                                    )
                                },
                            ));
                        }
                    };
                } else {
                    return Err(azure_core::Error::with_message(
                        azure_core::error::ErrorKind::Other,
                        || {
                            format!(
                                "Auto-detect could not find a cloud name from the current environment.",
                            )
                        },
                    ));
                }
            }
        };
        Ok(url::Url::parse(&url)?)
    }

    /// Returns the storage credentials for this cloud location,
    ///
    pub fn credentials(&self) -> &StorageCredentials {
        match self {
            CloudLocation::Public { credentials, .. }
            | CloudLocation::China { credentials, .. }
            | CloudLocation::USGov { credentials, .. }
            | CloudLocation::Custom { credentials, .. }
            | CloudLocation::AutoDetect { credentials, .. } => credentials,
            CloudLocation::Emulator { .. } => &EMULATOR_CREDENTIALS,
        }
    }

    /// Finds the cloud name, first by environment variable, then by parsing the current user's $HOME/.azure/config file
    ///
    fn find_cloud_name() -> Option<String> {
        if let Ok(name) = std::env::var("AZURE_CLOUD_NAME") {
            Some(name)
        } else if let Ok(home_dir) = std::env::var("HOME") {
            if let Some(config) = PathBuf::from(home_dir)
                .join(".azure/config")
                .canonicalize()
                .ok()
                .and_then(|config| File::open(config).ok())
            {
                let mut lines = BufReader::new(config).lines();

                while let Some(Ok(line)) = lines.next() {
                    if line.trim() == "[cloud]" {
                        if let Some(Ok(name)) = lines.next() {
                            if let Some((name, value)) = name.split_once('=') {
                                if name.trim() == "name" {
                                    return Some(value.trim().to_string());
                                }
                            }
                        }
                    }
                }
            }
            None
        } else {
            None
        }
    }
}

impl TryFrom<&Url> for CloudLocation {
    type Error = azure_core::Error;

    // TODO: Only supports Public, China, USGov, and Emulator
    // Is CustomURL required?
    // ref: https://github.com/Azure/azure-sdk-for-rust/issues/502
    fn try_from(url: &Url) -> azure_core::Result<Self> {
        let token = url.query().ok_or_else(|| {
            azure_core::Error::with_message(azure_core::error::ErrorKind::DataConversion, || {
                "unable to find SAS token in URL"
            })
        })?;
        let credentials = StorageCredentials::sas_token(token)?;

        let host = url.host_str().ok_or_else(|| {
            azure_core::Error::with_message(azure_core::error::ErrorKind::DataConversion, || {
                "unable to find the target host in the URL"
            })
        })?;

        let mut domain = host.split_terminator('.').collect::<Vec<_>>();
        if domain.len() < 2 {
            return Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::DataConversion,
                || {
                    format!(
                        "URL refers to a domain that is not a Public or China domain: {}",
                        host
                    )
                },
            ));
        }

        let account = domain.remove(0).to_string();
        domain.remove(0);
        let rest = domain.join(".");

        match rest.as_str() {
            "core.windows.net" => Ok(CloudLocation::Public {
                account,
                credentials,
            }),
            "core.chinacloudapi.cn" => Ok(CloudLocation::China {
                account,
                credentials,
            }),
            "core.usgovcloudapi.net" => Ok(CloudLocation::USGov {
                account,
                credentials,
            }),
            _ if url
                .path()
                .trim_start_matches('/')
                .starts_with(EMULATOR_ACCOUNT)
                && url.has_host()
                && url.port().is_some() =>
            {
                if let Some(host) = url.host() {
                    match host {
                        url::Host::Ipv4(ip) => Ok(CloudLocation::Emulator {
                            address: format!("{ip}"),
                            port: url.port().expect("should have a port"),
                        }),
                        _ => Err(azure_core::Error::with_message(
                            azure_core::error::ErrorKind::DataConversion,
                            || format!("Unsupported emulator URL, expected ipv4: {}", host),
                        )),
                    }
                } else {
                    unreachable!()
                }
            }
            _ => Err(azure_core::Error::with_message(
                azure_core::error::ErrorKind::DataConversion,
                || {
                    format!(
                        "URL refers to a domain that is not a Emulator, Public, China, or USGov domain: {}",
                        host
                    )
                },
            )),
        }
    }
}

pub static EMULATOR_CREDENTIALS: Lazy<StorageCredentials> = Lazy::new(|| {
    StorageCredentials::Key(EMULATOR_ACCOUNT.to_owned(), EMULATOR_ACCOUNT_KEY.to_owned())
});

/// The well-known account used by Azurite and the legacy Azure Storage Emulator.
/// <https://docs.microsoft.com/azure/storage/common/storage-use-azurite#well-known-storage-account-and-key>
pub const EMULATOR_ACCOUNT: &str = "devstoreaccount1";

/// The well-known account key used by Azurite and the legacy Azure Storage Emulator.
/// <https://docs.microsoft.com/azure/storage/common/storage-use-azurite#well-known-storage-account-and-key>
pub const EMULATOR_ACCOUNT_KEY: &str =
    "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_url() -> azure_core::Result<()> {
        let public_without_token = Url::parse("https://test.blob.core.windows.net")?;
        let public_with_token = Url::parse("https://test.blob.core.windows.net/?token=1")?;

        let cloud_location: CloudLocation = (&public_with_token).try_into()?;
        assert_eq!(public_without_token, cloud_location.url(ServiceType::Blob)?);

        let creds = cloud_location.credentials();
        assert!(matches!(creds, &StorageCredentials::SASToken(_)));

        let file_url = Url::parse("file://tmp/test.txt")?;
        let result: azure_core::Result<CloudLocation> = (&file_url).try_into();
        assert!(result.is_err());

        let missing_account = Url::parse("https://blob.core.windows.net?token=1")?;
        let result: azure_core::Result<CloudLocation> = (&missing_account).try_into();
        assert!(result.is_err());

        let missing_service_type = Url::parse("https://core.windows.net?token=1")?;
        let result: azure_core::Result<CloudLocation> = (&missing_service_type).try_into();
        assert!(result.is_err());

        let china_cloud = Url::parse("https://test.blob.core.chinacloudapi.cn/?token=1")?;
        let china_cloud_without_token = Url::parse("https://test.blob.core.chinacloudapi.cn")?;

        let cloud_location: CloudLocation = (&china_cloud).try_into()?;
        assert_eq!(
            china_cloud_without_token,
            cloud_location.url(ServiceType::Blob)?
        );

        let us_gov_cloud = Url::parse("https://test.blob.core.usgovcloudapi.net/?token=1")?;
        let us_gov_cloud_without_token = Url::parse("https://test.blob.core.usgovcloudapi.net")?;

        let cloud_location: CloudLocation = (&us_gov_cloud).try_into()?;
        assert_eq!(
            us_gov_cloud_without_token,
            cloud_location.url(ServiceType::Blob)?
        );

        let emulator =
            Url::parse(format!("http://127.0.0.1:5555/{EMULATOR_ACCOUNT}/?token=1").as_str())?;
        let emulator_without_token =
            Url::parse(format!("http://127.0.0.1:5555/{EMULATOR_ACCOUNT}").as_str())?;

        let cloud_location: CloudLocation = (&emulator).try_into()?;
        assert_eq!(
            emulator_without_token,
            cloud_location.url(ServiceType::Blob)?
        );

        // A SAS Url could contain a container name in the path, tests that the account is parsed successfully
        let emulator_with_container = Url::parse(
            format!("http://127.0.0.1:5555/{EMULATOR_ACCOUNT}/test_container?token=1").as_str(),
        )?;
        let emulator_with_container_without_token =
            Url::parse(format!("http://127.0.0.1:5555/{EMULATOR_ACCOUNT}").as_str())?;

        let cloud_location: CloudLocation = (&emulator_with_container).try_into()?;
        assert_eq!(
            emulator_with_container_without_token,
            cloud_location.url(ServiceType::Blob)?
        );

        Ok(())
    }

    #[test]
    fn test_auto_detect() {
        let cloud_location: CloudLocation =
            CloudLocation::auto_detect("test_account", StorageCredentials::Anonymous);

        std::env::set_var("AZURE_CLOUD_NAME", AZURE_US_GOV);
        assert_eq!(
            cloud_location
                .url(ServiceType::Blob)
                .expect("should return a url")
                .as_str(),
            "https://test_account.blob.core.usgovcloudapi.net/"
        );

        std::env::set_var("AZURE_CLOUD_NAME", AZURE_CHINA_CLOUD);
        assert_eq!(
            cloud_location
                .url(ServiceType::Blob)
                .expect("should return a url")
                .as_str(),
            "https://test_account.blob.core.chinacloudapi.cn/"
        );

        std::env::set_var("AZURE_CLOUD_NAME", AZURE_CLOUD);
        assert_eq!(
            cloud_location
                .url(ServiceType::Blob)
                .expect("should return a url")
                .as_str(),
            "https://test_account.blob.core.windows.net/"
        );

        std::env::set_var("AZURE_CLOUD_NAME", AZURE_PUBLIC_CLOUD);
        assert_eq!(
            cloud_location
                .url(ServiceType::Blob)
                .expect("should return a url")
                .as_str(),
            "https://test_account.blob.core.windows.net/"
        );

        std::env::set_var("AZURE_CLOUD_NAME", "NotACloud");
        assert!(cloud_location.url(ServiceType::Blob).is_err());

        std::env::remove_var("AZURE_CLOUD_NAME");

        let test_dir = std::env::temp_dir().join("test_cloud_location_auto_detect");
        std::env::set_var("HOME", test_dir.as_os_str());
        let test_azure_dir = test_dir.join(".azure");
        std::fs::create_dir_all(&test_azure_dir).expect("should be able to create test dir");

        let config_file = test_azure_dir.join("config");
        std::fs::write(
            &config_file,
            r#"
[cloud]
name = AzureCloud
            "#
            .trim(),
        )
        .expect("should be able to write test config file");
        assert_eq!(
            cloud_location
                .url(ServiceType::Blob)
                .expect("should return a url")
                .as_str(),
            "https://test_account.blob.core.windows.net/"
        );

        std::fs::write(
            &config_file,
            r#"
[cloud]
name = AzureChinaCloud
            "#
            .trim(),
        )
        .expect("should be able to write test config file");
        assert_eq!(
            cloud_location
                .url(ServiceType::Blob)
                .expect("should return a url")
                .as_str(),
            "https://test_account.blob.core.chinacloudapi.cn/"
        );

        std::fs::write(
            &config_file,
            r#"
[cloud]
name = AzureUSGovernment
            "#
            .trim(),
        )
        .expect("should be able to write test config file");
        assert_eq!(
            cloud_location
                .url(ServiceType::Blob)
                .expect("should return a url")
                .as_str(),
            "https://test_account.blob.core.usgovcloudapi.net/"
        );

        std::fs::write(
            &config_file,
            r#"
            "#
            .trim(),
        )
        .expect("should be able to write test config file");
        assert!(cloud_location.url(ServiceType::Blob).is_err());

        // Clean-up test files
        std::fs::remove_dir_all(test_dir).expect("should be able to remove test dir");
    }
}
