#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use std::os::unix::fs::PermissionsExt;

use saka_core::ids::ProjectId;
use saka_core::secret::SecretText;
use saka_env::AeadSecrets;
use saka_ports::{Sealed, SecretStore};
use uuid::Uuid;

fn project() -> ProjectId {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    ProjectId(Uuid::from_u128(u128::from(
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )))
}

fn value() -> SecretText {
    SecretText::new("npm-token-0123456789".into())
}

fn key_file() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("saka").join("secret.key");
    (dir, path)
}

#[test]
fn a_sealed_value_opens_only_for_its_project_and_name() {
    let store = AeadSecrets::from_key(&[7u8; 32]);
    let (a, b) = (project(), project());
    let sealed = store.seal(a, "NPM_TOKEN", &value()).unwrap();
    assert_eq!(sealed.nonce.len(), 24);
    assert!(
        !sealed.ciphertext.windows(8).any(|w| w == b"npm-toke"),
        "the ciphertext hides the value"
    );
    assert_eq!(store.open(a, "NPM_TOKEN", &sealed).unwrap(), value());

    for (project, name) in [(b, "NPM_TOKEN"), (a, "OTHER_TOKEN")] {
        let e = store.open(project, name, &sealed).unwrap_err();
        assert_eq!((e.code.as_str(), e.retryable), ("secret_unreadable", false));
    }
    let mut tampered = sealed.clone();
    tampered.ciphertext[0] ^= 1;
    assert!(store.open(a, "NPM_TOKEN", &tampered).is_err());
    let short = Sealed {
        nonce: vec![0; 12],
        ciphertext: sealed.ciphertext.clone(),
    };
    assert!(store.open(a, "NPM_TOKEN", &short).is_err());
    let other_key = AeadSecrets::from_key(&[8u8; 32]);
    assert!(other_key.open(a, "NPM_TOKEN", &sealed).is_err());

    let again = store.seal(a, "NPM_TOKEN", &value()).unwrap();
    assert_ne!(again.nonce, sealed.nonce, "every write gets a fresh nonce");
}

#[test]
fn the_fingerprint_is_keyed_and_bound_to_project_name_and_value() {
    let store = AeadSecrets::from_key(&[7u8; 32]);
    let p = project();
    let fp = store.fingerprint(p, "NPM_TOKEN", &value());
    assert_eq!(fp.len(), 64);
    assert_eq!(fp, store.fingerprint(p, "NPM_TOKEN", &value()));
    assert!(!fp.contains("npm"));
    for other in [
        store.fingerprint(project(), "NPM_TOKEN", &value()),
        store.fingerprint(p, "API_TOKEN", &value()),
        store.fingerprint(p, "NPM_TOKEN", &SecretText::new("another-value-1".into())),
        AeadSecrets::from_key(&[8u8; 32]).fingerprint(p, "NPM_TOKEN", &value()),
    ] {
        assert_ne!(other, fp);
    }
}

#[test]
fn a_missing_key_file_is_created_0600_and_a_loose_one_refuses_start() {
    let (_dir, path) = key_file();
    let first = AeadSecrets::from_key_file(&path).unwrap();
    let meta = std::fs::metadata(&path).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(meta.len(), 32);
    let p = project();
    let sealed = first.seal(p, "NPM_TOKEN", &value()).unwrap();
    let reopened = AeadSecrets::from_key_file(&path).unwrap();
    assert_eq!(reopened.open(p, "NPM_TOKEN", &sealed).unwrap(), value());

    for mode in [0o644, 0o640, 0o400, 0o700] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        let e = AeadSecrets::from_key_file(&path).err().unwrap();
        assert_eq!(e.code, "config_key_file_mode", "{mode:o}");
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&path, [1u8; 16]).unwrap();
    let e = AeadSecrets::from_key_file(&path).err().unwrap();
    assert_eq!(e.code, "config_key_file_invalid");
    assert!(!e.detail.contains('\u{1}'));

    let (_dir, link) = key_file();
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&path, &link).unwrap();
    let e = AeadSecrets::from_key_file(&link).err().unwrap();
    assert_eq!(e.code, "config_key_file_invalid");
}
