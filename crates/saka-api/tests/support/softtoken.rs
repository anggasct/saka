use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use serde_cbor_2::Value as Cbor;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const USER_PRESENT: u8 = 0x01;
const USER_VERIFIED: u8 = 0x04;
const ATTESTED: u8 = 0x40;

pub struct SoftToken {
    key: SigningKey,
    credential_id: Vec<u8>,
    pub counter: u32,
    pub rp_id: String,
    pub origin: String,
}

fn url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn challenge_of(options: &Value) -> String {
    options["publicKey"]["challenge"]
        .as_str()
        .unwrap()
        .to_string()
}

impl SoftToken {
    pub fn new(seed: u8, rp_id: &str, origin: &str) -> SoftToken {
        let mut secret = [seed; 32];
        secret[0] = 1;
        SoftToken {
            key: SigningKey::from_slice(&secret).unwrap(),
            credential_id: vec![seed; 32],
            counter: 0,
            rp_id: rp_id.to_string(),
            origin: origin.to_string(),
        }
    }

    fn rp_hash(&self) -> Vec<u8> {
        Sha256::digest(self.rp_id.as_bytes()).to_vec()
    }

    fn client_data(&self, kind: &str, challenge: &str) -> Vec<u8> {
        json!({
            "type": kind,
            "challenge": challenge,
            "origin": self.origin,
            "crossOrigin": false,
        })
        .to_string()
        .into_bytes()
    }

    fn cose_key(&self) -> Vec<u8> {
        let point = self.key.verifying_key().to_sec1_point(false);
        let bytes = point.as_bytes();
        let (x, y) = (bytes[1..33].to_vec(), bytes[33..65].to_vec());
        let mut map = BTreeMap::new();
        map.insert(Cbor::Integer(1), Cbor::Integer(2));
        map.insert(Cbor::Integer(3), Cbor::Integer(-7));
        map.insert(Cbor::Integer(-1), Cbor::Integer(1));
        map.insert(Cbor::Integer(-2), Cbor::Bytes(x));
        map.insert(Cbor::Integer(-3), Cbor::Bytes(y));
        serde_cbor_2::to_vec(&Cbor::Map(map)).unwrap()
    }

    pub fn create(&mut self, options: &Value) -> Value {
        let client_data = self.client_data("webauthn.create", &challenge_of(options));
        let mut auth_data = self.rp_hash();
        auth_data.push(USER_PRESENT | USER_VERIFIED | ATTESTED);
        auth_data.extend_from_slice(&self.counter.to_be_bytes());
        auth_data.extend_from_slice(&[0u8; 16]);
        auth_data.extend_from_slice(
            &u16::try_from(self.credential_id.len())
                .unwrap()
                .to_be_bytes(),
        );
        auth_data.extend_from_slice(&self.credential_id);
        auth_data.extend_from_slice(&self.cose_key());
        let mut attestation = BTreeMap::new();
        attestation.insert(Cbor::Text("fmt".into()), Cbor::Text("none".into()));
        attestation.insert(Cbor::Text("attStmt".into()), Cbor::Map(BTreeMap::new()));
        attestation.insert(Cbor::Text("authData".into()), Cbor::Bytes(auth_data));
        let attestation_object = serde_cbor_2::to_vec(&Cbor::Map(attestation)).unwrap();
        json!({
            "id": url(&self.credential_id),
            "rawId": url(&self.credential_id),
            "type": "public-key",
            "response": {
                "attestationObject": url(&attestation_object),
                "clientDataJSON": url(&client_data),
            },
            "extensions": {},
        })
    }

    pub fn get(&mut self, options: &Value) -> Value {
        let client_data = self.client_data("webauthn.get", &challenge_of(options));
        let mut auth_data = self.rp_hash();
        auth_data.push(USER_PRESENT | USER_VERIFIED);
        auth_data.extend_from_slice(&self.counter.to_be_bytes());
        let mut signed = auth_data.clone();
        signed.extend_from_slice(&Sha256::digest(&client_data));
        let signature: Signature = self.key.sign(&signed);
        json!({
            "id": url(&self.credential_id),
            "rawId": url(&self.credential_id),
            "type": "public-key",
            "response": {
                "authenticatorData": url(&auth_data),
                "clientDataJSON": url(&client_data),
                "signature": url(signature.to_der().as_bytes()),
                "userHandle": Value::Null,
            },
            "extensions": {},
        })
    }
}
