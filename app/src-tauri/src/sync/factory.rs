use crate::{
    error::{AppError, Result},
    models::{CloudBackendKind, CloudCredentialInput, CloudSyncSettings},
    sync::{
        backend::{CloudBackend, RemotePath},
        credentials::{
            CredentialStore, SecretKind, StoredCloudCredentialProfile,
            load_or_migrate_credential_bundle,
        },
        s3::S3Backend,
        webdav::WebDavBackend,
    },
};
use std::sync::Arc;
use url::Url;

pub fn backend_from_input(
    settings: &CloudSyncSettings,
    credentials: &CloudCredentialInput,
) -> Result<Arc<dyn CloudBackend>> {
    match (&settings.backend, credentials) {
        (CloudBackendKind::Webdav, CloudCredentialInput::Webdav { password, .. }) => Ok(Arc::new(
            WebDavBackend::new(&webdav_base_url(settings)?, &settings.username, password)
                .map_err(cloud_configuration)?,
        )),
        (
            CloudBackendKind::S3,
            CloudCredentialInput::S3 {
                access_key_id,
                secret_access_key,
                session_token,
                ..
            },
        ) => Ok(Arc::new(
            S3Backend::new(
                &settings.s3,
                access_key_id,
                secret_access_key,
                session_token.as_deref(),
            )
            .map_err(cloud_configuration)?,
        )),
        _ => Err(AppError::Configuration(
            "cloud credentials do not match the selected backend".into(),
        )),
    }
}

pub async fn backend_from_store<S: CredentialStore + ?Sized>(
    settings: &CloudSyncSettings,
    credentials: &S,
) -> Result<Arc<dyn CloudBackend>> {
    if let Some(bundle) = load_or_migrate_credential_bundle(credentials, settings).await? {
        return backend_from_profile(settings, &bundle.active);
    }
    match settings.backend {
        CloudBackendKind::Webdav => {
            let password = required_secret(
                credentials,
                &settings.remote_id,
                SecretKind::WebDavPassword,
                "WebDAV password is not configured",
            )
            .await?;
            Ok(Arc::new(
                WebDavBackend::new(
                    &webdav_base_url(settings)?,
                    &settings.username,
                    password.expose_secret(),
                )
                .map_err(cloud_configuration)?,
            ))
        }
        CloudBackendKind::S3 => {
            let access_key_id = required_secret(
                credentials,
                &settings.remote_id,
                SecretKind::S3AccessKeyId,
                "S3 Access Key ID is not configured",
            )
            .await?;
            let secret_access_key = required_secret(
                credentials,
                &settings.remote_id,
                SecretKind::S3SecretAccessKey,
                "S3 Secret Access Key is not configured",
            )
            .await?;
            let session_token = credentials
                .get(&settings.remote_id, SecretKind::S3SessionToken)
                .await?;
            Ok(Arc::new(
                S3Backend::new(
                    &settings.s3,
                    access_key_id.expose_secret(),
                    secret_access_key.expose_secret(),
                    session_token.as_ref().map(|value| value.expose_secret()),
                )
                .map_err(cloud_configuration)?,
            ))
        }
    }
}

pub fn backend_from_profile(
    settings: &CloudSyncSettings,
    profile: &StoredCloudCredentialProfile,
) -> Result<Arc<dyn CloudBackend>> {
    match (&settings.backend, profile) {
        (CloudBackendKind::Webdav, StoredCloudCredentialProfile::Webdav { password, .. }) => {
            Ok(Arc::new(
                WebDavBackend::new(&webdav_base_url(settings)?, &settings.username, password)
                    .map_err(cloud_configuration)?,
            ))
        }
        (
            CloudBackendKind::S3,
            StoredCloudCredentialProfile::S3 {
                access_key_id,
                secret_access_key,
                session_token,
                ..
            },
        ) => Ok(Arc::new(
            S3Backend::new(
                &settings.s3,
                access_key_id,
                secret_access_key,
                session_token.as_deref(),
            )
            .map_err(cloud_configuration)?,
        )),
        _ => Err(AppError::Credential(
            "stored cloud credentials do not match the selected backend".into(),
        )),
    }
}

async fn required_secret<S: CredentialStore + ?Sized>(
    credentials: &S,
    remote_id: &str,
    kind: SecretKind,
    message: &'static str,
) -> Result<crate::sync::credentials::SecretValue> {
    credentials
        .get(remote_id, kind)
        .await?
        .ok_or_else(|| AppError::Credential(message.into()))
}

fn webdav_base_url(settings: &CloudSyncSettings) -> Result<String> {
    let mut url = Url::parse(&settings.base_url)
        .map_err(|_| AppError::Configuration("invalid WebDAV URL".into()))?;
    let root = settings.root_path.trim().trim_matches('/');
    let path =
        RemotePath::parse(root).map_err(|error| AppError::Configuration(error.to_string()))?;
    if !path.segments().is_empty() {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| AppError::Configuration("invalid WebDAV base URL".into()))?;
        segments.pop_if_empty();
        for segment in path.segments() {
            segments.push(segment);
        }
        segments.push("");
    }
    Ok(url.into())
}

fn cloud_configuration(error: crate::sync::backend::CloudError) -> AppError {
    AppError::Configuration(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{backend_from_input, backend_from_store};
    use crate::{
        models::{CloudBackendKind, CloudCredentialInput, CloudSyncSettings, S3CloudSyncSettings},
        sync::{
            credentials::{
                CREDENTIAL_BUNDLE_VERSION, CredentialStore, MemoryCredentialStore, SecretKind,
                SecretValue, StoredCloudCredentialProfile, StoredCredentialBundle,
                load_credential_bundle, save_credential_bundle,
            },
            test_s3_server::TestS3,
            test_server::TestWebDav,
        },
    };

    #[tokio::test]
    async fn draft_factory_selects_webdav_without_persisting_credentials() {
        let server = TestWebDav::start("user", "pass").await;
        let settings = CloudSyncSettings {
            backend: CloudBackendKind::Webdav,
            base_url: server.endpoint().into(),
            username: "user".into(),
            ..CloudSyncSettings::default()
        };
        let credentials = CloudCredentialInput::Webdav {
            password: "pass".into(),
            sync_password: None,
        };

        backend_from_input(&settings, &credentials)
            .unwrap()
            .test_capabilities()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn stored_factory_selects_s3_credentials_by_remote_identity() {
        let server = TestS3::start("AKID", None).await;
        let settings = CloudSyncSettings {
            backend: CloudBackendKind::S3,
            remote_id: "remote-s3".into(),
            s3: S3CloudSyncSettings {
                endpoint_url: server.endpoint().into(),
                region: "us-east-1".into(),
                bucket: "archive".into(),
                prefix: "factory".into(),
                force_path_style: true,
            },
            ..CloudSyncSettings::default()
        };
        let store = MemoryCredentialStore::default();
        store
            .set(
                "remote-s3",
                SecretKind::S3AccessKeyId,
                SecretValue::new("AKID"),
            )
            .await
            .unwrap();
        store
            .set(
                "remote-s3",
                SecretKind::S3SecretAccessKey,
                SecretValue::new("secret-key"),
            )
            .await
            .unwrap();

        backend_from_store(&settings, &store)
            .await
            .unwrap()
            .test_capabilities()
            .await
            .unwrap();
        let migrated = load_credential_bundle(&store, "remote-s3")
            .await
            .unwrap()
            .expect("legacy credentials should be migrated into one atomic bundle");
        assert!(matches!(
            migrated.active,
            StoredCloudCredentialProfile::S3 {
                ref access_key_id,
                ref secret_access_key,
                session_token: None,
                sync_passphrase: None,
            } if access_key_id == "AKID" && secret_access_key == "secret-key"
        ));
        for kind in [
            SecretKind::S3AccessKeyId,
            SecretKind::S3SecretAccessKey,
            SecretKind::S3SessionToken,
            SecretKind::SyncPassphrase,
        ] {
            assert!(store.get("remote-s3", kind).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn stored_factory_prefers_the_atomic_bundle_over_legacy_secret_entries() {
        let server = TestS3::start("BUNDLE-AKID", Some("bundle-token")).await;
        let settings = CloudSyncSettings {
            backend: CloudBackendKind::S3,
            remote_id: "remote-bundle".into(),
            s3: S3CloudSyncSettings {
                endpoint_url: server.endpoint().into(),
                region: "us-east-1".into(),
                bucket: "archive".into(),
                prefix: "factory-bundle".into(),
                force_path_style: true,
            },
            ..CloudSyncSettings::default()
        };
        let store = MemoryCredentialStore::default();
        store
            .set(
                "remote-bundle",
                SecretKind::S3AccessKeyId,
                SecretValue::new("STALE-AKID"),
            )
            .await
            .unwrap();
        store
            .set(
                "remote-bundle",
                SecretKind::S3SecretAccessKey,
                SecretValue::new("stale-secret"),
            )
            .await
            .unwrap();
        save_credential_bundle(
            &store,
            "remote-bundle",
            &StoredCredentialBundle {
                version: CREDENTIAL_BUNDLE_VERSION,
                active: StoredCloudCredentialProfile::S3 {
                    access_key_id: "BUNDLE-AKID".into(),
                    secret_access_key: "secret-key".into(),
                    session_token: Some("bundle-token".into()),
                    sync_passphrase: None,
                },
                pending: None,
            },
        )
        .await
        .unwrap();

        backend_from_store(&settings, &store)
            .await
            .unwrap()
            .test_capabilities()
            .await
            .unwrap();
    }

    #[test]
    fn draft_factory_rejects_credentials_for_another_backend() {
        let settings = CloudSyncSettings::default();
        let credentials = CloudCredentialInput::S3 {
            access_key_id: "AKID".into(),
            secret_access_key: "secret".into(),
            session_token: None,
            sync_password: None,
        };

        assert!(backend_from_input(&settings, &credentials).is_err());
    }
}
