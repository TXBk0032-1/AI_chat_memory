use super::super::validate_encryption_credentials;
use crate::models::{CloudCredentialInput, CloudSyncSettings};
use crate::sync::vault::VaultProtection;

#[test]
fn sync_protector_is_stable_within_a_vault_and_separated_between_vaults() {
    let first_policy = VaultProtection::encrypted("vault-a", "shared password").unwrap();
    let other_policy = VaultProtection::encrypted("vault-b", "shared password").unwrap();
    let first = first_policy
        .derive_protector("vault-a", "shared password")
        .unwrap()
        .unwrap();
    let second = first_policy
        .derive_protector("vault-a", "shared password")
        .unwrap()
        .unwrap();
    let other_vault = other_policy
        .derive_protector("vault-b", "shared password")
        .unwrap()
        .unwrap();
    let nonce = [7_u8; 24];
    let ciphertext = first.seal(b"header", b"payload", nonce).unwrap();

    assert_eq!(
        second.open(b"header", &ciphertext, nonce).unwrap(),
        b"payload"
    );
    assert!(other_vault.open(b"header", &ciphertext, nonce).is_err());
}

#[test]
fn encrypted_connection_requires_a_sync_passphrase() {
    let settings = CloudSyncSettings {
        encryption_enabled: true,
        ..CloudSyncSettings::default()
    };
    let missing = CloudCredentialInput::Webdav {
        password: "webdav".into(),
        sync_password: None,
    };
    let present = CloudCredentialInput::Webdav {
        password: "webdav".into(),
        sync_password: Some("shared".into()),
    };

    assert!(validate_encryption_credentials(&settings, &missing).is_err());
    assert!(validate_encryption_credentials(&settings, &present).is_ok());
}
