use std::sync::Arc;

use crate::{
    models::S3CloudSyncSettings,
    sync::{
        backend::{CloudBackend, RemotePath},
        crypto::{Argon2idConfig, PayloadProtector},
        s3::S3Backend,
        test_s3_server::TestS3,
        vault::{VaultDocument, VaultIdentity, VaultProtection, load_or_create_vault},
    },
};

pub(crate) fn fast_kdf_config() -> Argon2idConfig {
    Argon2idConfig {
        salt: [11; 16],
        memory_kib: 8 * 1024,
        iterations: 1,
        parallelism: 1,
    }
}

pub(crate) fn test_s3_backend(server: &TestS3, prefix: &str) -> Arc<S3Backend> {
    let settings = S3CloudSyncSettings {
        endpoint_url: server.endpoint().into(),
        region: "us-east-1".into(),
        bucket: "archive".into(),
        prefix: prefix.into(),
        force_path_style: true,
    };
    Arc::new(S3Backend::new(&settings, "AKID", "secret-key", None).unwrap())
}

pub(crate) fn test_protection(vault_id: &str, passphrase: &str) -> VaultProtection {
    VaultProtection::encrypted_with_config(vault_id, passphrase, fast_kdf_config()).unwrap()
}

pub(crate) fn test_protector(vault_id: &str, passphrase: &str) -> Arc<dyn PayloadProtector> {
    test_protection(vault_id, passphrase)
        .derive_protector(vault_id, passphrase)
        .unwrap()
        .unwrap()
}

pub(crate) async fn initialize_test_vault<B: CloudBackend + ?Sized>(
    backend: &B,
    vault_id: &str,
    generation_id: &str,
    protection: VaultProtection,
) {
    load_or_create_vault(
        backend,
        VaultDocument::active(
            VaultIdentity {
                format_version: 2,
                vault_id: vault_id.to_owned(),
                generation_id: generation_id.to_owned(),
            },
            protection,
        ),
    )
    .await
    .unwrap();
}

pub(crate) async fn assert_generation_layout(backend: &dyn CloudBackend, expected: &[&str]) {
    let generations = backend
        .list_depth_one(&RemotePath::parse("v1/generations").unwrap())
        .await
        .unwrap();
    let mut actual = generations
        .into_iter()
        .filter(|entry| entry.is_collection)
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    actual.sort();
    let mut expected_sorted = expected.to_vec();
    expected_sorted.sort();
    assert_eq!(actual, expected_sorted);
}
