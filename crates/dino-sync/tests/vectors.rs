//! dino-sync's own published test vectors (`tests/vectors.json`), for anyone implementing the
//! protocol elsewhere: encrypted records, device approval and the recovery key.
//!
//! `DINO_SYNC_WRITE_VECTORS=1 cargo test -p dino-sync --test vectors -- --ignored` writes a new
//! file; encryption nonces are random, so tokens change but must keep opening.

use dino_sync::approval::{DeviceKeys, Grant, approval_code};
use dino_sync::crypto::AccountKey;
use dino_sync::record::{Record, RecordId};
use dino_sync::recovery::RecoveryKey;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use serde_json::{Value, json};

const ACCOUNT: &str = "acct_0001";

fn account_key() -> AccountKey {
    AccountKey::from_bytes("k_0102030405060708", std::array::from_fn(|i| i as u8))
}

fn approver() -> DeviceKeys {
    DeviceKeys::from_secret(std::array::from_fn(|i| 0x40 + i as u8))
}

fn newcomer() -> DeviceKeys {
    DeviceKeys::from_secret(std::array::from_fn(|i| 0x80 + i as u8))
}

fn recovery() -> RecoveryKey {
    RecoveryKey::generate_with(&mut ChaCha20Rng::from_seed([7; 32]))
}

#[test]
#[ignore]
fn write_vectors() {
    if std::env::var("DINO_SYNC_WRITE_VECTORS").as_deref() != Ok("1") {
        return;
    }
    let key = account_key();
    let records: Vec<Value> = [(RecordId::new("agents", "claude.mode"), 1, "\"plan\""), (RecordId::new("keys", "OPENROUTER_API_KEY"), 1, "\"sk-or-v1-example\"")]
        .into_iter()
        .map(|(id, schema, plain)| json!({"account": ACCOUNT, "collection": id.collection, "key": id.key, "schema": schema, "plaintext": plain, "token": key.seal(ACCOUNT, &id, schema, plain).unwrap()}))
        .collect();
    let grant = approver().grant(&newcomer().public(), ACCOUNT, &key, &mut ChaCha20Rng::from_seed([9; 32])).unwrap();
    let r = recovery();
    let file = json!({
        "about": "dino-sync test vectors. Records are PASETO v4.local with footer {\"kid\":…} and implicit assertion JSON [account, collection, key, schema].",
        "account_key": {"id": key.id, "hex": hex(key.bytes())},
        "records": records,
        "approval": {
            "account": ACCOUNT,
            "approver_secret_hex": hex(approver().secret_bytes().as_ref()),
            "approver_public": approver().public(),
            "new_device_secret_hex": hex(newcomer().secret_bytes().as_ref()),
            "new_device_public": newcomer().public(),
            "code": approval_code(ACCOUNT, &newcomer().public(), &approver().public()),
            "grant": grant,
        },
        "recovery": {
            "account": ACCOUNT,
            "display": r.display(),
            "also_accepted": [r.display().to_lowercase(), r.display().replace('-', " "), r.display().replace('0', "O")],
            "wrapped": r.wrap(ACCOUNT, &key).unwrap(),
        },
    });
    std::fs::write(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors.json"), serde_json::to_string_pretty(&file).unwrap() + "\n").unwrap();
}

#[test]
fn published_vectors_hold() {
    let v: Value = serde_json::from_str(include_str!("vectors.json")).unwrap();
    let key = account_key();
    assert_eq!(v["account_key"]["id"], key.id.as_str());
    assert_eq!(v["account_key"]["hex"], hex(key.bytes()).as_str());

    for r in v["records"].as_array().unwrap() {
        let id = RecordId::new(r["collection"].as_str().unwrap(), r["key"].as_str().unwrap());
        let schema = r["schema"].as_u64().unwrap() as u32;
        let token = r["token"].as_str().unwrap();
        assert_eq!(key.open(ACCOUNT, &id, schema, token).unwrap(), r["plaintext"].as_str().unwrap());
        // Bound to its place: another setting, schema or account doesn't open it.
        assert!(key.open(ACCOUNT, &RecordId::new("agents", "codex.mode"), schema, token).is_err());
        assert!(key.open(ACCOUNT, &id, schema + 1, token).is_err());
        assert!(key.open("acct_other", &id, schema, token).is_err());
    }

    let a = &v["approval"];
    assert_eq!(a["approver_public"], approver().public().as_str());
    assert_eq!(a["new_device_public"], newcomer().public().as_str());
    assert_eq!(a["code"], approval_code(ACCOUNT, &newcomer().public(), &approver().public()).as_str());
    let grant: Grant = serde_json::from_value(a["grant"].clone()).unwrap();
    // The box is deterministic for a given nonce: the same inputs make the same grant.
    let again = approver().grant(&newcomer().public(), ACCOUNT, &key, &mut ChaCha20Rng::from_seed([9; 32])).unwrap();
    assert_eq!(grant, again);
    let got = newcomer().accept(&grant, &approver().public(), ACCOUNT).unwrap();
    assert_eq!((got.id.as_str(), got.bytes()), (key.id.as_str(), key.bytes()));

    let rec = &v["recovery"];
    let r = RecoveryKey::parse(rec["display"].as_str().unwrap()).unwrap();
    assert_eq!(r, recovery());
    for typed in rec["also_accepted"].as_array().unwrap() {
        assert_eq!(RecoveryKey::parse(typed.as_str().unwrap()).unwrap(), r);
    }
    let got = r.unwrap(ACCOUNT, rec["wrapped"].as_str().unwrap()).unwrap();
    assert_eq!(got.bytes(), key.bytes());
}

#[test]
fn approval_refuses_a_swapped_key_or_account() {
    let key = account_key();
    let (a, n, mallory) = (approver(), newcomer(), DeviceKeys::from_secret([0x11; 32]));
    let grant = a.grant(&n.public(), ACCOUNT, &key, &mut ChaCha20Rng::from_seed([1; 32])).unwrap();
    // A grant claiming to be from someone else, for another account, or opened by another key.
    assert!(n.accept(&grant, &mallory.public(), ACCOUNT).is_err());
    assert!(n.accept(&grant, &a.public(), "acct_other").is_err());
    assert!(mallory.accept(&grant, &a.public(), ACCOUNT).is_err());
    let forged = mallory.grant(&n.public(), ACCOUNT, &AccountKey::generate(), &mut ChaCha20Rng::from_seed([2; 32])).unwrap();
    assert!(n.accept(&Grant { from: a.public(), ..forged }, &a.public(), ACCOUNT).is_err());
    // Swapping either key changes the code both screens show.
    let code = approval_code(ACCOUNT, &n.public(), &a.public());
    assert_ne!(code, approval_code(ACCOUNT, &mallory.public(), &a.public()));
    assert_ne!(code, approval_code(ACCOUNT, &n.public(), &mallory.public()));
    assert_eq!(code.len(), 8);
}

#[test]
fn recovery_catches_typos() {
    let shown = recovery().display();
    assert_eq!(shown.len(), "D1-".len() + 28 + 6);
    // Every single-character change to the key part is caught.
    let alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut caught = 0;
    for (i, c) in shown.char_indices().skip(3).filter(|(_, c)| *c != '-') {
        for d in alphabet.chars().filter(|d| *d != c) {
            let mut t = shown.clone();
            t.replace_range(i..i + 1, &d.to_string());
            assert!(RecoveryKey::parse(&t).is_err() || RecoveryKey::parse(&t).unwrap() != recovery());
            caught += RecoveryKey::parse(&t).is_err() as usize;
        }
    }
    // 10 check bits: a random mistype slips through about 1 time in 1024.
    let total = 28 * 31;
    assert!(caught * 100 >= total * 99, "caught {caught} of {total}");
    assert!(RecoveryKey::parse("D1-0000").is_err());
    assert!(RecoveryKey::parse(&shown.replacen("D1", "D2", 1)).is_err());
    assert!(recovery().unwrap(ACCOUNT, &RecoveryKey::generate().wrap(ACCOUNT, &account_key()).unwrap()).is_err());
}

#[test]
fn values_are_bound_to_their_key_id() {
    let key = account_key();
    let token = key.seal(ACCOUNT, &RecordId::new("policies", "allow_bypass"), 1, "false").unwrap();
    assert_eq!(dino_sync::crypto::key_id(&token).unwrap(), key.id);
    let rotated = AccountKey::from_bytes("k_rotated", *key.bytes());
    assert!(matches!(rotated.open(ACCOUNT, &RecordId::new("policies", "allow_bypass"), 1, &token), Err(dino_sync::CryptoError::UnknownKey(_))));
    assert!(key.seal(ACCOUNT, &RecordId::new("policies", "allow_bypass"), 1, "").is_err());
}

#[test]
fn unknown_fields_round_trip() {
    let wire = json!({
        "collection": "agents", "key": "claude.mode",
        "hlc": {"wall_ms": 1790000000000u64, "counter": 2, "device": "d_a", "future_hlc_field": 1},
        "schema": 2, "value": "v4.local.xyz", "seq": 41,
        "written_by_version": "0.9.0", "labels": ["x"]
    });
    let r: Record = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(r.extra.get("written_by_version"), Some(&json!("0.9.0")));
    let back = serde_json::to_value(&r).unwrap();
    let mut expected = wire.clone();
    expected["hlc"].as_object_mut().unwrap().remove("future_hlc_field");
    assert_eq!(back, expected);

    let pull = json!({"seq": 41, "records": [wire], "more": false, "server_hint": {"x": 1}});
    let p: dino_sync::PullResponse = serde_json::from_value(pull.clone()).unwrap();
    assert_eq!(serde_json::to_value(&p).unwrap()["server_hint"], json!({"x": 1}));

    let n: dino_sync::Nudge = serde_json::from_value(json!({"type": "something_new", "x": 1})).unwrap();
    assert_eq!(n, dino_sync::Nudge::Unknown);
    let n: dino_sync::Nudge = serde_json::from_value(json!({"type": "advanced", "seq": 7})).unwrap();
    assert_eq!(n, dino_sync::Nudge::Advanced { seq: 7 });
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
