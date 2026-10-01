//! dino-sync's own published test vectors (`tests/vectors.json`), for anyone implementing the
//! protocol elsewhere: encrypted records, device approval and the recovery key.
//!
//! `DINO_SYNC_WRITE_VECTORS=1 cargo test -p dino-sync --test vectors -- --ignored` writes a new
//! file; encryption and approval nonces are random, so tokens, the commitment and the code change
//! but must keep holding.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use dino_sync::approval::{Commitment, DeviceKeys, Grant, Response, Reveal, approval_code, verify_reveal};
use dino_sync::crypto::{AccountKey, CryptoError};
use dino_sync::hlc::Hlc;
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

fn stamp(wall_ms: u64, counter: u32, device: &str) -> Hlc {
    Hlc { wall_ms, counter, device: device.into() }
}

#[test]
#[ignore]
fn write_vectors() {
    if std::env::var("DINO_SYNC_WRITE_VECTORS").as_deref() != Ok("1") {
        return;
    }
    let key = account_key();
    let records: Vec<Value> = [
        (RecordId::new("agents", "claude.mode"), stamp(1790000000000, 0, "d_approver"), Some("\"plan\"")),
        (RecordId::new("keys", "OPENROUTER_API_KEY"), stamp(1790000000123, 2, "d_a"), Some("\"sk-or-v1-example\"")),
        (RecordId::new("policies", "allow_bypass"), stamp(1790000000456, 0, "d_b"), None),
    ]
    .into_iter()
    .map(|(id, hlc, plain)| json!({"account": ACCOUNT, "plaintext": plain, "record": key.seal_record(ACCOUNT, id, hlc, 1, plain).unwrap()}))
    .collect();
    let (commitment, reveal) = newcomer().commit(ACCOUNT, &mut ChaCha20Rng::from_seed([3; 32]));
    let response = approver().respond(&mut ChaCha20Rng::from_seed([4; 32]));
    let grant = approver().grant(&newcomer().public(), ACCOUNT, &key, &mut ChaCha20Rng::from_seed([9; 32])).unwrap();
    let r = recovery();
    let file = json!({
        "about": "dino-sync test vectors, protocol 2. Records are PASETO v4.local with footer {\"kid\":…} and implicit assertion JSON [account, collection, key, schema, wall_ms, counter, device, deleted]; a delete seals the payload \"deleted\". Approval: commitment = BLAKE2b-256 and code = BLAKE2b-64 mod 10^6, each over length-prefixed (u64 LE) parts, as in src/approval.rs.",
        "account_key": {"id": key.id, "hex": hex(key.bytes())},
        "records": records,
        "approval": {
            "account": ACCOUNT,
            "approver_secret_hex": hex(approver().secret_bytes().as_ref()),
            "approver_public": approver().public(),
            "new_device_secret_hex": hex(newcomer().secret_bytes().as_ref()),
            "new_device_public": newcomer().public(),
            "new_device_nonce": reveal.nonce,
            "commitment": commitment.hash,
            "approver_nonce": response.nonce,
            "code": approval_code(ACCOUNT, &reveal, &response).unwrap(),
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

    let records = v["records"].as_array().unwrap();
    assert!(records.iter().any(|r| r["record"]["deleted"] == true), "a delete is among the vectors");
    for r in records {
        let record: Record = serde_json::from_value(r["record"].clone()).unwrap();
        let plaintext = r["plaintext"].as_str().map(str::to_string);
        assert_eq!(record.is_tombstone(), plaintext.is_none());
        assert_eq!(key.open_record(ACCOUNT, &record), Ok(plaintext));
        // Bound to everything the record says: another setting, schema, stamp, delete flag or
        // account doesn't open it.
        let h = &record.hlc;
        let tampered = [
            Record { id: RecordId::new("agents", "codex.mode"), ..record.clone() },
            Record { schema: record.schema + 1, ..record.clone() },
            Record { hlc: Hlc { wall_ms: h.wall_ms + 1, ..h.clone() }, ..record.clone() },
            Record { hlc: Hlc { counter: h.counter + 1, ..h.clone() }, ..record.clone() },
            Record { hlc: Hlc { device: "d_other".into(), ..h.clone() }, ..record.clone() },
            Record { deleted: !record.deleted, ..record.clone() },
        ];
        for t in &tampered {
            assert_eq!(key.open_record(ACCOUNT, t), Err(CryptoError::Invalid), "{t:?}");
        }
        assert_eq!(key.open_record("acct_other", &record), Err(CryptoError::Invalid));
    }

    let a = &v["approval"];
    assert_eq!(a["approver_public"], approver().public().as_str());
    assert_eq!(a["new_device_public"], newcomer().public().as_str());
    let reveal = Reveal { public: newcomer().public(), nonce: a["new_device_nonce"].as_str().unwrap().into() };
    let response = Response { public: approver().public(), nonce: a["approver_nonce"].as_str().unwrap().into() };
    let commitment = Commitment { hash: a["commitment"].as_str().unwrap().into() };
    assert_eq!(Commitment::of(ACCOUNT, &reveal).unwrap(), commitment);
    verify_reveal(ACCOUNT, &commitment, &reveal).unwrap();
    assert_eq!(a["code"], approval_code(ACCOUNT, &reveal, &response).unwrap().as_str());
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

/// JSON through the server and back, as each message travels.
fn relay<T: serde::Serialize + serde::de::DeserializeOwned>(message: &T) -> T {
    serde_json::from_str(&serde_json::to_string(message).unwrap()).unwrap()
}

#[test]
fn approval_in_order_gives_both_screens_the_same_code() {
    let key = account_key();
    let (a, n) = (approver(), newcomer());
    // 1. The new device commits; it keeps the reveal back.
    let (commitment, reveal) = n.commit(ACCOUNT, &mut ChaCha20Rng::from_seed([5; 32]));
    let at_approver = relay(&commitment);
    // 2. The approver answers with its key and a fresh nonce.
    let response = a.respond(&mut ChaCha20Rng::from_seed([6; 32]));
    let at_new = relay(&response);
    // 3. Only now the new device reveals; 4. the approver checks it, and both show the code.
    let revealed = relay(&reveal);
    verify_reveal(ACCOUNT, &at_approver, &revealed).unwrap();
    let on_approver = approval_code(ACCOUNT, &revealed, &response).unwrap();
    let on_new = approval_code(ACCOUNT, &reveal, &at_new).unwrap();
    assert_eq!(on_approver, on_new);
    assert_eq!(on_approver.len(), 8);
    // 5. The person matched the codes: the grant goes to the revealed key, from the responding one.
    let grant = relay(&a.grant(&revealed.public, ACCOUNT, &key, &mut ChaCha20Rng::from_seed([7; 32])).unwrap());
    let got = n.accept(&grant, &at_new.public, ACCOUNT).unwrap();
    assert_eq!(got.bytes(), key.bytes());
    // Every commitment has its own nonce.
    let (again, _) = n.commit(ACCOUNT, &mut ChaCha20Rng::from_seed([8; 32]));
    assert_ne!(again, commitment);
}

#[test]
fn a_key_swapped_after_the_commitment_is_caught() {
    let v: Value = serde_json::from_str(include_str!("vectors.json")).unwrap();
    let a = &v["approval"];
    let mallory = DeviceKeys::from_secret([0x11; 32]);
    let reveal = Reveal { public: newcomer().public(), nonce: a["new_device_nonce"].as_str().unwrap().into() };
    let response = Response { public: approver().public(), nonce: a["approver_nonce"].as_str().unwrap().into() };
    let commitment = Commitment::of(ACCOUNT, &reveal).unwrap();
    // The server relays the commitment, sees the approver's nonce, then swaps in a key of its own
    // (chosen, with its nonce, so the code comes out the same): the commitment gives it away.
    let swapped = Reveal { public: mallory.public(), ..reveal.clone() };
    assert_eq!(verify_reveal(ACCOUNT, &commitment, &swapped), Err(CryptoError::Invalid));
    let renonced = Reveal { nonce: response.nonce.clone(), ..reveal.clone() };
    assert_eq!(verify_reveal(ACCOUNT, &commitment, &renonced), Err(CryptoError::Invalid));
    assert_eq!(verify_reveal("acct_other", &commitment, &reveal), Err(CryptoError::Invalid));
    assert!(verify_reveal(ACCOUNT, &Commitment { hash: "not base64!".into() }, &reveal).is_err());
    // A commitment of its own made up front verifies, but then its code is a one-in-a-million
    // guess made before the approver's nonce existed.
    let (m_commitment, m_reveal) = mallory.commit(ACCOUNT, &mut ChaCha20Rng::from_seed([1; 32]));
    verify_reveal(ACCOUNT, &m_commitment, &m_reveal).unwrap();
    assert_eq!(verify_reveal(ACCOUNT, &m_commitment, &reveal), Err(CryptoError::Invalid));
}

#[test]
fn every_key_and_nonce_changes_the_code() {
    let v: Value = serde_json::from_str(include_str!("vectors.json")).unwrap();
    let a = &v["approval"];
    let mallory = DeviceKeys::from_secret([0x11; 32]).public();
    let reveal = Reveal { public: newcomer().public(), nonce: a["new_device_nonce"].as_str().unwrap().into() };
    let response = Response { public: approver().public(), nonce: a["approver_nonce"].as_str().unwrap().into() };
    let code = approval_code(ACCOUNT, &reveal, &response).unwrap();
    let flip = |nonce: &str| {
        let mut b = B64.decode(nonce).unwrap();
        b[0] ^= 1;
        B64.encode(b)
    };
    // Each checked against the Python reference that wrote the vectors: none collides.
    let changed = [
        approval_code(ACCOUNT, &Reveal { nonce: flip(reveal.nonce.as_str()), ..reveal.clone() }, &response),
        approval_code(ACCOUNT, &reveal, &Response { nonce: flip(response.nonce.as_str()), ..response.clone() }),
        approval_code(ACCOUNT, &Reveal { public: mallory.clone(), ..reveal.clone() }, &response),
        approval_code(ACCOUNT, &reveal, &Response { public: mallory.clone(), ..response.clone() }),
        approval_code("acct_other", &reveal, &response),
        approval_code(ACCOUNT, &Reveal { public: response.public.clone(), nonce: response.nonce.clone() }, &Response { public: reveal.public.clone(), nonce: reveal.nonce.clone() }),
    ];
    for c in changed {
        assert_ne!(c.unwrap(), code);
    }
    // Malformed nonces and one key in both roles don't make a code at all.
    assert!(approval_code(ACCOUNT, &Reveal { nonce: "AAAA".into(), ..reveal.clone() }, &response).is_err());
    assert!(approval_code(ACCOUNT, &reveal, &Response { public: reveal.public.clone(), ..response.clone() }).is_err());
}

#[test]
fn small_order_keys_are_refused() {
    let key = account_key();
    let (a, n) = (approver(), newcomer());
    let order_8: [u8; 32] = [
        0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f, 0xc4, 0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16, 0x5f, 0x49, 0xb8, 0x00,
    ];
    let mut zero_high_bit = [0u8; 32];
    zero_high_bit[31] = 0x80;
    let mut p_minus_1 = [0xffu8; 32];
    p_minus_1[0] = 0xec;
    p_minus_1[31] = 0x7f;
    let mut one = [0u8; 32];
    one[0] = 1;
    let grant = a.grant(&n.public(), ACCOUNT, &key, &mut ChaCha20Rng::from_seed([1; 32])).unwrap();
    let response = a.respond(&mut ChaCha20Rng::from_seed([2; 32]));
    for bad in [[0u8; 32], one, order_8, p_minus_1, zero_high_bit] {
        let bad = B64.encode(bad);
        assert!(a.grant(&bad, ACCOUNT, &key, &mut ChaCha20Rng::from_seed([3; 32])).is_err());
        assert!(n.accept(&Grant { from: bad.clone(), ..grant.clone() }, &bad, ACCOUNT).is_err());
        let reveal = Reveal { public: bad.clone(), nonce: response.nonce.clone() };
        assert!(Commitment::of(ACCOUNT, &reveal).is_err());
        assert!(approval_code(ACCOUNT, &reveal, &response).is_err());
        assert!(approval_code(ACCOUNT, &Reveal { public: n.public(), nonce: response.nonce.clone() }, &Response { public: bad, ..response.clone() }).is_err());
    }
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
    let (id, hlc) = (RecordId::new("policies", "allow_bypass"), stamp(1790000000000, 0, "d_a"));
    let token = key.seal(ACCOUNT, &id, &hlc, 1, Some("false")).unwrap();
    assert_eq!(dino_sync::crypto::key_id(&token).unwrap(), key.id);
    let rotated = AccountKey::from_bytes("k_rotated", *key.bytes());
    assert!(matches!(rotated.open(ACCOUNT, &id, &hlc, 1, false, &token), Err(CryptoError::UnknownKey(_))));
    assert_eq!(key.seal(ACCOUNT, &id, &hlc, 1, Some("")), Err(CryptoError::Empty));
    // A delete is a token too, under the same key.
    let deleted = key.seal(ACCOUNT, &id, &hlc, 1, None).unwrap();
    assert_eq!(dino_sync::crypto::key_id(&deleted).unwrap(), key.id);
    assert_eq!(key.open(ACCOUNT, &id, &hlc, 1, true, &deleted), Ok(None));
    assert_eq!(key.open(ACCOUNT, &id, &hlc, 1, false, &deleted), Err(CryptoError::Invalid));
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
    assert!(!r.is_tombstone());
    let back = serde_json::to_value(&r).unwrap();
    let mut expected = wire.clone();
    expected["hlc"].as_object_mut().unwrap().remove("future_hlc_field");
    assert_eq!(back, expected);

    let mut deleted = wire.clone();
    deleted["deleted"] = json!(true);
    let d: Record = serde_json::from_value(deleted).unwrap();
    assert!(d.is_tombstone());
    assert_eq!(serde_json::to_value(&d).unwrap()["deleted"], json!(true));

    let pull = json!({"seq": 41, "records": [wire], "more": false, "server_hint": {"x": 1}});
    let p: dino_sync::PullResponse = serde_json::from_value(pull.clone()).unwrap();
    assert_eq!(serde_json::to_value(&p).unwrap()["server_hint"], json!({"x": 1}));
    assert_eq!(dino_sync::record::check_pull(&p), Ok(()));

    let n: dino_sync::Nudge = serde_json::from_value(json!({"type": "something_new", "x": 1})).unwrap();
    assert_eq!(n, dino_sync::Nudge::Unknown);
    let n: dino_sync::Nudge = serde_json::from_value(json!({"type": "advanced", "seq": 7})).unwrap();
    assert_eq!(n, dino_sync::Nudge::Advanced { seq: 7 });
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
