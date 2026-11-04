#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use std::os::unix::fs::PermissionsExt;

use saka_core::ids::{ProjectId, UserLinkAuthorizationId, UserLinkId};
use saka_core::secret::{SecretScope, SecretText};
use saka_core::vocab::UserTokenKind;
use saka_env::AeadSecrets;
use saka_ports::{Sealed, SecretStore};
use uuid::Uuid;

fn project() -> ProjectId {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    ProjectId(Uuid::from_u128(u128::from(
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )))
}

fn scope(project_id: ProjectId, name: &str) -> SecretScope<'_> {
    SecretScope::Project { project_id, name }
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
    let sealed = store.seal(&scope(a, "NPM_TOKEN"), &value()).unwrap();
    assert_eq!(sealed.nonce.len(), 24);
    assert!(
        !sealed.ciphertext.windows(8).any(|w| w == b"npm-toke"),
        "the ciphertext hides the value"
    );
    assert_eq!(
        store.open(&scope(a, "NPM_TOKEN"), &sealed).unwrap(),
        value()
    );

    for (project, name) in [(b, "NPM_TOKEN"), (a, "OTHER_TOKEN")] {
        let e = store.open(&scope(project, name), &sealed).unwrap_err();
        assert_eq!((e.code.as_str(), e.retryable), ("secret_unreadable", false));
    }
    let mut tampered = sealed.clone();
    tampered.ciphertext[0] ^= 1;
    assert!(store.open(&scope(a, "NPM_TOKEN"), &tampered).is_err());
    let short = Sealed {
        nonce: vec![0; 12],
        ciphertext: sealed.ciphertext.clone(),
    };
    assert!(store.open(&scope(a, "NPM_TOKEN"), &short).is_err());
    let other_key = AeadSecrets::from_key(&[8u8; 32]);
    assert!(other_key.open(&scope(a, "NPM_TOKEN"), &sealed).is_err());

    let again = store.seal(&scope(a, "NPM_TOKEN"), &value()).unwrap();
    assert_ne!(again.nonce, sealed.nonce, "every write gets a fresh nonce");
}

#[test]
fn the_fingerprint_is_keyed_and_bound_to_project_name_and_value() {
    let store = AeadSecrets::from_key(&[7u8; 32]);
    let p = project();
    let fp = store.fingerprint(&scope(p, "NPM_TOKEN"), &value());
    assert_eq!(fp.len(), 64);
    assert_eq!(fp, store.fingerprint(&scope(p, "NPM_TOKEN"), &value()));
    assert!(!fp.contains("npm"));
    for other in [
        store.fingerprint(&scope(project(), "NPM_TOKEN"), &value()),
        store.fingerprint(&scope(p, "API_TOKEN"), &value()),
        store.fingerprint(
            &scope(p, "NPM_TOKEN"),
            &SecretText::new("another-value-1".into()),
        ),
        AeadSecrets::from_key(&[8u8; 32]).fingerprint(&scope(p, "NPM_TOKEN"), &value()),
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
    let sealed = first.seal(&scope(p, "NPM_TOKEN"), &value()).unwrap();
    let reopened = AeadSecrets::from_key_file(&path).unwrap();
    assert_eq!(
        reopened.open(&scope(p, "NPM_TOKEN"), &sealed).unwrap(),
        value()
    );

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

#[test]
fn a_value_sealed_under_the_project_aad_of_earlier_versions_still_opens() {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};
    let key = [7u8; 32];
    let p = project();
    let mut aad = p.0.to_string().into_bytes();
    aad.push(0);
    aad.extend_from_slice(b"NPM_TOKEN");
    let nonce = [3u8; 24];
    let ciphertext = XChaCha20Poly1305::new(&key.into())
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: b"npm-token-0123456789",
                aad: &aad,
            },
        )
        .unwrap();
    let sealed = Sealed {
        nonce: nonce.to_vec(),
        ciphertext,
    };
    let store = AeadSecrets::from_key(&key);
    assert_eq!(
        store.open(&scope(p, "NPM_TOKEN"), &sealed).unwrap(),
        value()
    );
}

#[test]
fn a_user_token_moved_to_another_link_row_or_scope_fails_to_open() {
    let store = AeadSecrets::from_key(&[7u8; 32]);
    let link = UserLinkId(Uuid::from_u128(41));
    let other = UserLinkId(Uuid::from_u128(42));
    let token = SecretText::new("ghu_access_token_0123456789".into());
    let access = SecretScope::UserLinkToken {
        link_id: link,
        kind: UserTokenKind::Access,
    };
    let sealed = store.seal(&access, &token).unwrap();
    assert_eq!(store.open(&access, &sealed).unwrap(), token);
    let id = link.0;
    for wrong in [
        SecretScope::UserLinkToken {
            link_id: other,
            kind: UserTokenKind::Access,
        },
        SecretScope::UserLinkToken {
            link_id: link,
            kind: UserTokenKind::Refresh,
        },
        SecretScope::DeviceCode {
            authorization_id: UserLinkAuthorizationId(id),
        },
        SecretScope::ProjectUserToken {
            project_id: ProjectId(id),
        },
        SecretScope::Project {
            project_id: ProjectId(id),
            name: "user_link",
        },
    ] {
        let e = store.open(&wrong, &sealed).unwrap_err();
        assert_eq!((e.code.as_str(), e.retryable), ("secret_unreadable", false));
        assert!(!e.params.to_string().contains("ghu_"));
    }
}
