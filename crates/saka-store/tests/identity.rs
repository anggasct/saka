#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use saka_core::identity::{Device, Pairing};
use saka_core::ids::{DeviceId, PairingId};
use saka_core::time::Timestamp;
use saka_core::vocab::{DeviceKind, PairingPurpose};
use saka_store::identity::{self, NewDevice, NewPairing};
use saka_store::{Batch, Store};
use uuid::Uuid;

const NOW: Timestamp = Timestamp(1_000_000);

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        &dir.path().join("saka.db"),
        &dir.path().join("backups"),
        NOW,
    )
    .unwrap();
    (dir, store)
}

fn device(n: u128, kind: DeviceKind, hash: &str) -> NewDevice {
    NewDevice {
        device: Device {
            id: DeviceId(Uuid::from_u128(n)),
            name: format!("device {n}"),
            kind,
            created_at: NOW,
            last_used_at: None,
            revoked_at: None,
        },
        token_sha256: hash.to_string(),
    }
}

fn pairing(n: u128, purpose: PairingPurpose, hash: &str, ttl: i64) -> NewPairing {
    NewPairing {
        pairing: Pairing {
            id: PairingId(Uuid::from_u128(n)),
            purpose,
            created_by: "operator:local".into(),
            created_at: NOW,
            expires_at: Timestamp(NOW.0 + ttl),
        },
        code_sha256: hash.to_string(),
    }
}

fn commit(store: &Store, f: impl FnOnce(&mut Batch)) {
    let mut batch = Batch::new(NOW);
    f(&mut batch);
    store.commit(&batch).unwrap();
}

#[test]
fn a_device_is_found_by_its_token_hash_until_it_is_revoked() {
    let (_dir, store) = store();
    commit(&store, |b| {
        b.new_devices.push(device(1, DeviceKind::Client, "h1"));
        b.new_devices.push(device(2, DeviceKind::LocalCli, "h2"));
    });
    let found = store.read(|c| identity::device_by_token(c, "h1")).unwrap();
    assert_eq!(found.unwrap().id, DeviceId(Uuid::from_u128(1)));
    assert!(
        store
            .read(|c| identity::device_by_token(c, "nope"))
            .unwrap()
            .is_none()
    );

    commit(&store, |b| {
        b.revoked_devices.push(DeviceId(Uuid::from_u128(1)));
        b.revoked_devices.push(DeviceId(Uuid::from_u128(2)));
    });
    assert!(
        store
            .read(|c| identity::device_by_token(c, "h1"))
            .unwrap()
            .is_none()
    );
    let local = store.read(|c| identity::device_by_token(c, "h2")).unwrap();
    assert!(local.is_some(), "the local CLI device is never revoked");
    let active = store.read(identity::active_devices).unwrap();
    assert_eq!(active.len(), 1);
}

#[test]
fn the_database_refuses_a_second_local_cli_device_and_a_duplicate_token() {
    let (_dir, store) = store();
    commit(&store, |b| {
        b.new_devices.push(device(1, DeviceKind::LocalCli, "h1"))
    });
    for bad in [
        device(2, DeviceKind::LocalCli, "h2"),
        device(3, DeviceKind::Client, "h1"),
    ] {
        let mut batch = Batch::new(NOW);
        batch.new_devices.push(bad);
        assert!(store.commit(&batch).is_err());
    }
}

#[test]
fn reissuing_the_local_cli_token_keeps_the_device_and_drops_the_old_hash() {
    let (_dir, store) = store();
    assert!(
        store
            .transact(|tx| identity::reissue_local_cli(tx, "h2", NOW))
            .unwrap()
            .is_none()
    );
    commit(&store, |b| {
        b.new_devices.push(device(1, DeviceKind::LocalCli, "h1"))
    });
    let id = store
        .transact(|tx| identity::reissue_local_cli(tx, "h2", NOW))
        .unwrap();
    assert_eq!(id, Some(DeviceId(Uuid::from_u128(1))));
    assert!(
        store
            .read(|c| identity::device_by_token(c, "h1"))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .read(|c| identity::device_by_token(c, "h2"))
            .unwrap()
            .is_some()
    );
}

#[test]
fn touching_a_device_writes_at_most_once_a_minute() {
    let (_dir, store) = store();
    commit(&store, |b| {
        b.new_devices.push(device(1, DeviceKind::Client, "h1"))
    });
    let id = DeviceId(Uuid::from_u128(1));
    let used = |store: &Store| {
        store
            .read(|c| identity::device(c, id))
            .unwrap()
            .unwrap()
            .last_used_at
    };
    store
        .transact(|tx| identity::touch_device(tx, id, Timestamp(10_000)))
        .unwrap();
    store
        .transact(|tx| identity::touch_device(tx, id, Timestamp(20_000)))
        .unwrap();
    assert_eq!(used(&store), Some(Timestamp(10_000)));
    store
        .transact(|tx| identity::touch_device(tx, id, Timestamp(70_000)))
        .unwrap();
    assert_eq!(used(&store), Some(Timestamp(70_000)));
}

#[test]
fn a_code_is_claimed_once_for_its_purpose_while_it_lives() {
    let (_dir, store) = store();
    commit(&store, |b| {
        b.new_pairings
            .push(pairing(1, PairingPurpose::Token, "c1", 300_000));
        b.new_pairings
            .push(pairing(2, PairingPurpose::Setup, "c2", 900_000));
    });
    let claim = |hash: &str, purposes: &[PairingPurpose], at: i64| {
        store
            .transact(|tx| identity::claim_pairing(tx, hash, purposes, Timestamp(at)))
            .unwrap()
    };
    assert!(
        claim(
            "c1",
            &[PairingPurpose::Setup, PairingPurpose::Passkey],
            NOW.0
        )
        .is_none()
    );
    let got = claim("c1", &[PairingPurpose::Token], NOW.0 + 1).unwrap();
    assert_eq!(
        (got.id, got.purpose),
        (PairingId(Uuid::from_u128(1)), PairingPurpose::Token)
    );
    assert!(
        claim("c1", &[PairingPurpose::Token], NOW.0 + 2).is_none(),
        "used"
    );
    assert!(claim("c2", &[PairingPurpose::Setup], NOW.0 + 899_999).is_some());
}

#[test]
fn an_expired_code_cannot_be_claimed_and_the_boundary_is_exclusive() {
    let (_dir, store) = store();
    commit(&store, |b| {
        b.new_pairings
            .push(pairing(1, PairingPurpose::Token, "c1", 300_000));
        b.new_pairings
            .push(pairing(2, PairingPurpose::Token, "c2", 300_000));
    });
    let claim = |hash: &str, at: i64| {
        store
            .transact(|tx| {
                identity::claim_pairing(tx, hash, &[PairingPurpose::Token], Timestamp(at))
            })
            .unwrap()
    };
    assert!(claim("c1", NOW.0 + 300_000).is_none());
    assert!(claim("c2", NOW.0 + 299_999).is_some());
}

#[test]
fn five_wrong_guesses_void_every_live_code_of_that_purpose_only() {
    let (_dir, store) = store();
    commit(&store, |b| {
        b.new_pairings
            .push(pairing(1, PairingPurpose::Token, "c1", 300_000));
        b.new_pairings
            .push(pairing(2, PairingPurpose::Setup, "c2", 900_000));
    });
    let claim = |hash: &str, purpose: PairingPurpose| {
        store
            .transact(|tx| identity::claim_pairing(tx, hash, &[purpose], Timestamp(NOW.0 + 1)))
            .unwrap()
    };
    for _ in 0..5 {
        assert!(claim("wrong", PairingPurpose::Token).is_none());
    }
    assert!(claim("c1", PairingPurpose::Token).is_none());
    assert!(
        claim("c2", PairingPurpose::Setup).is_some(),
        "other purposes keep their attempts"
    );
}

#[test]
fn old_codes_are_pruned_when_a_new_one_is_stored() {
    let (_dir, store) = store();
    commit(&store, |b| {
        b.new_pairings
            .push(pairing(1, PairingPurpose::Token, "old", 1_000))
    });
    let later = Timestamp(NOW.0 + 2 * 24 * 60 * 60 * 1000);
    let mut batch = Batch::new(later);
    batch.new_pairings.push(NewPairing {
        pairing: Pairing {
            created_at: later,
            expires_at: Timestamp(later.0 + 300_000),
            ..pairing(2, PairingPurpose::Token, "new", 0).pairing
        },
        code_sha256: "new".into(),
    });
    store.commit(&batch).unwrap();
    let rows: i64 = store
        .read(|c| Ok(c.query_row("SELECT count(*) FROM pairing_code", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn setup_is_required_until_a_passkey_exists() {
    let (_dir, store) = store();
    assert_eq!(store.read(identity::active_credentials).unwrap(), 0);
    store
        .transact(|tx| {
            tx.execute(
                "INSERT INTO credential (id, name, passkey, created_at) VALUES ('00000000-0000-0000-0000-00000000000c', 'Laptop', '{}', 1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(store.read(identity::active_credentials).unwrap(), 1);
    assert_eq!(
        store.read(identity::active_credential_list).unwrap().len(),
        1
    );
}
