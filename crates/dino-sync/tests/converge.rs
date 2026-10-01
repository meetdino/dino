//! Three devices editing the same settings, offline and online, with changes delivered late, out of
//! order and more than once: once everything has reached everyone, all three hold the same state.
//! And what a server without the account key can't do to them.

use dino_sync::crypto::AccountKey;
use dino_sync::hlc::{Clock, FutureStamp, Hlc, MAX_SKEW_MS};
use dino_sync::merge::{ApplyError, Store, WriteError};
use dino_sync::record::{Record, RecordId};
use dino_sync::CryptoError;
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use serde_json::Map;

const DEVICES: usize = 3;
const KEYS: [&str; 4] = ["claude.mode", "claude.model", "codex.mode", "allow_bypass"];
const ACCOUNT: &str = "acct_0001";
const NOW: u64 = 1_700_000_000_000;

#[derive(Debug, Clone)]
enum Step {
    /// Device `0` sets key `1` to `2` (None: deletes it), its wall clock moved by `3` ms (it can go back).
    Edit(usize, usize, Option<u8>, i64),
    /// Device `0` receives the first `1` changes it hasn't seen yet, in the order given by seed `2`.
    Deliver(usize, usize, u64),
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0..DEVICES, 0..KEYS.len(), proptest::option::of(any::<u8>()), -2_000i64..3_000).prop_map(|(d, k, v, dt)| Step::Edit(d, k, v, dt)),
        (0..DEVICES, 0usize..6, any::<u64>()).prop_map(|(d, n, s)| Step::Deliver(d, n, s)),
    ]
}

struct Device {
    clock: Clock,
    store: Store,
    wall: i64,
    /// Indexes into the log this device has received.
    seen: Vec<bool>,
}

fn shuffle<T>(v: &mut [T], mut seed: u64) {
    for i in (1..v.len()).rev() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        v.swap(i, (seed >> 33) as usize % (i + 1));
    }
}

/// A record as a device writes it: sealed with `key`, or (to test the merge alone, fast) a stand-in
/// token the server-side merge never opens.
fn write(key: Option<&AccountKey>, id: RecordId, hlc: Hlc, value: Option<String>) -> Record {
    match key {
        Some(key) => key.seal_record(ACCOUNT, id, hlc, 1, value.as_deref()).unwrap(),
        None => Record { id, hlc, schema: 1, deleted: value.is_none(), value: value.unwrap_or_else(|| "deleted".into()), seq: None, extra: Map::new() },
    }
}

fn deliver(dev: &mut Device, log: &[Record], mut picks: Vec<usize>, seed: u64, key: Option<&AccountKey>) {
    shuffle(&mut picks, seed);
    for i in picks {
        let now = dev.wall.max(0) as u64;
        match key {
            Some(key) => {
                dev.store.apply_verified(key, ACCOUNT, log[i].clone(), now).expect("an honest record opens");
            }
            None => {
                dev.store.apply_unverified(log[i].clone());
            }
        }
        dev.clock.observe(&log[i].hlc, now).expect("devices' clocks are minutes apart at most");
        dev.seen[i] = true;
    }
}

fn state(s: &Store) -> Vec<(RecordId, String, bool, String)> {
    s.iter().map(|r| (r.id.clone(), r.value.clone(), r.deleted, format!("{:?}", r.hlc))).collect()
}

fn converge(steps: Vec<Step>, skews: [i64; DEVICES], key: Option<&AccountKey>) -> Result<(), TestCaseError> {
    let mut devs: Vec<Device> = (0..DEVICES).map(|i| Device { clock: Clock::new(format!("d{i}")), store: Store::new(), wall: NOW as i64 + skews[i], seen: vec![] }).collect();
    let mut log: Vec<Record> = vec![];
    for s in steps {
        match s {
            Step::Edit(d, k, v, dt) => {
                let dev = &mut devs[d];
                dev.wall += dt;
                let hlc = dev.clock.now(dev.wall.max(0) as u64);
                let r = write(key, RecordId::new("agents", KEYS[k]), hlc, v.map(|v| format!("v{v}")));
                dev.store.write_local(r.clone(), 1).expect("a fresh local stamp beats everything this device has seen");
                log.push(r);
                for dv in devs.iter_mut() { dv.seen.resize(log.len(), false); }
                devs[d].seen[log.len() - 1] = true;
            }
            Step::Deliver(d, n, seed) => {
                let picks: Vec<usize> = (0..log.len()).filter(|i| !devs[d].seen[*i]).take(n).collect();
                deliver(&mut devs[d], &log, picks, seed, key);
            }
        }
    }
    // Everything reaches everyone, again, in each device's own order (duplicates are harmless).
    for (i, dev) in devs.iter_mut().enumerate() {
        dev.seen.resize(log.len(), false);
        deliver(dev, &log, (0..log.len()).collect(), i as u64 * 7919 + 1, key);
    }
    let first = state(&devs[0].store);
    for dev in &devs[1..] {
        prop_assert_eq!(&state(&dev.store), &first);
    }
    // And it's the latest write for each key.
    for k in KEYS {
        let latest = log.iter().filter(|r| r.id.key == k).max_by(|a, b| a.hlc.cmp(&b.hlc));
        prop_assert_eq!(devs[0].store.get(&RecordId::new("agents", k)).map(|r| &r.value), latest.map(|r| &r.value));
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn any_interleaving_converges(steps in proptest::collection::vec(step(), 1..60), skews in proptest::array::uniform3(-5_000i64..5_000)) {
        converge(steps, skews, None)?;
    }
}

proptest! {
    // Real encryption is slower: fewer cases, the same property through `apply_verified`.
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn any_interleaving_of_sealed_records_converges(steps in proptest::collection::vec(step(), 1..60), skews in proptest::array::uniform3(-5_000i64..5_000)) {
        converge(steps, skews, Some(&account_key()))?;
    }
}

fn account_key() -> AccountKey {
    AccountKey::from_bytes("k_0102030405060708", std::array::from_fn(|i| i as u8))
}

fn stamp(wall_ms: u64, counter: u32, device: &str) -> Hlc {
    Hlc { wall_ms, counter, device: device.into() }
}

fn bypass() -> RecordId {
    RecordId::new("policies", "allow_bypass")
}

#[test]
fn a_newer_schema_is_kept_and_not_overwritten() {
    let key = account_key();
    let mut store = Store::new();
    let mut newer = Clock::new("new");
    let r = key.seal_record(ACCOUNT, RecordId::new("agents", "claude.mode"), newer.now(NOW), 2, Some("{\"newshape\":1}")).unwrap();
    assert_eq!(store.apply_verified(&key, ACCOUNT, r.clone(), NOW), Ok(true));
    assert!(!store.readable(&r.id, 1));
    let mut older = Clock::new("old");
    older.observe(&r.hlc, NOW).unwrap();
    let mine = key.seal_record(ACCOUNT, r.id.clone(), older.now(NOW + 1000), 1, Some("\"plan\"")).unwrap();
    assert_eq!(store.write_local(mine, 1), Err(WriteError::NewerSchema { stored: 2, ours: 1 }));
    assert_eq!(store.get(&r.id), Some(&r));
}

#[test]
fn a_sealed_delete_applies() {
    let key = account_key();
    let mut store = Store::new();
    let set = key.seal_record(ACCOUNT, bypass(), stamp(NOW, 0, "d_a"), 1, Some("false")).unwrap();
    let delete = key.seal_record(ACCOUNT, bypass(), stamp(NOW + 1, 0, "d_a"), 1, None).unwrap();
    assert!(delete.is_tombstone());
    assert_eq!(key.open_record(ACCOUNT, &set), Ok(Some("false".to_string())));
    assert_eq!(key.open_record(ACCOUNT, &delete), Ok(None));
    let (changed, refused) = store.apply_all_verified(&key, ACCOUNT, [set, delete.clone()], NOW);
    assert_eq!((changed.len(), refused.len()), (2, 0));
    assert_eq!(store.get(&bypass()), Some(&delete));
}

#[test]
fn the_server_cant_forge_a_delete() {
    let key = account_key();
    let mut store = Store::new();
    let set = key.seal_record(ACCOUNT, bypass(), stamp(NOW, 0, "d_a"), 1, Some("false")).unwrap();
    assert_eq!(store.apply_verified(&key, ACCOUNT, set.clone(), NOW), Ok(true));
    // Flipping the flag on a real token, or a made-up token, under a later stamp.
    let flipped = Record { hlc: stamp(NOW + 1, 0, "d_a"), deleted: true, ..set.clone() };
    assert_eq!(store.apply_verified(&key, ACCOUNT, flipped.clone(), NOW), Err(ApplyError::Crypto(CryptoError::Invalid)));
    assert_eq!(store.apply_verified(&key, ACCOUNT, Record { hlc: set.hlc.clone(), ..flipped.clone() }, NOW), Ok(false), "not later: nothing to check");
    let made_up = Record { value: "v4.local.AAAA".into(), ..flipped };
    assert!(store.apply_verified(&key, ACCOUNT, made_up, NOW).is_err());
    // A delete sealed for another setting doesn't move here either.
    let other = key.seal_record(ACCOUNT, RecordId::new("policies", "close_merged"), stamp(NOW + 2, 0, "d_a"), 1, None).unwrap();
    assert_eq!(store.apply_verified(&key, ACCOUNT, Record { id: bypass(), ..other }, NOW), Err(ApplyError::Crypto(CryptoError::Invalid)));
    assert_eq!(store.get(&bypass()), Some(&set));
}

#[test]
fn the_server_cant_restamp_or_replay() {
    let key = account_key();
    let mut store = Store::new();
    let old = key.seal_record(ACCOUNT, bypass(), stamp(NOW, 0, "d_a"), 1, Some("true")).unwrap();
    let new = key.seal_record(ACCOUNT, bypass(), stamp(NOW + 5, 0, "d_b"), 1, Some("false")).unwrap();
    assert_eq!(store.apply_verified(&key, ACCOUNT, new.clone(), NOW), Ok(true));
    // The old value replayed as it was loses to the stored one, the high-water mark...
    assert_eq!(store.apply_verified(&key, ACCOUNT, old.clone(), NOW), Ok(false));
    // ...and restamped to win, in any part of its stamp, it no longer opens.
    for hlc in [stamp(NOW + 6, 0, "d_a"), stamp(NOW + 5, 1, "d_b"), stamp(NOW + 5, 0, "d_c")] {
        assert_eq!(store.apply_verified(&key, ACCOUNT, Record { hlc, ..old.clone() }, NOW), Err(ApplyError::Crypto(CryptoError::Invalid)));
    }
    // Another value under the same stamp can't be made without the key.
    assert_eq!(store.apply_verified(&key, ACCOUNT, Record { value: old.value.clone(), ..new.clone() }, NOW), Ok(false));
    let mut fresh = Store::new();
    assert_eq!(fresh.apply_verified(&key, ACCOUNT, Record { value: old.value.clone(), ..new.clone() }, NOW), Err(ApplyError::Crypto(CryptoError::Invalid)));
    // Nor under another account.
    assert!(fresh.apply_verified(&key, "acct_other", new.clone(), NOW).is_err());
    assert!(fresh.is_empty());
    assert_eq!(store.get(&bypass()), Some(&new));
}

#[test]
fn a_made_up_schema_cant_lock_a_setting() {
    let key = account_key();
    let mut store = Store::new();
    let r = key.seal_record(ACCOUNT, bypass(), stamp(NOW, 0, "d_a"), 1, Some("false")).unwrap();
    let locked = Record { schema: u32::MAX, hlc: stamp(NOW + 1, 0, "d_a"), ..r.clone() };
    assert_eq!(store.apply_verified(&key, ACCOUNT, locked, NOW), Err(ApplyError::Crypto(CryptoError::Invalid)));
    assert!(store.get(&bypass()).is_none());
    let mine = key.seal_record(ACCOUNT, bypass(), stamp(NOW + 2, 0, "d_b"), 1, Some("true")).unwrap();
    assert_eq!(store.write_local(mine, 1), Ok(()));
}

#[test]
fn stamps_far_in_the_future_are_refused() {
    let key = account_key();
    let mut store = Store::new();
    // Even sealed by a device with the key: it would win every conflict for years.
    let ahead = MAX_SKEW_MS + 1;
    let r = key.seal_record(ACCOUNT, bypass(), stamp(NOW + ahead, 0, "d_a"), 1, Some("true")).unwrap();
    assert_eq!(store.apply_verified(&key, ACCOUNT, r.clone(), NOW), Err(ApplyError::FutureStamp(FutureStamp { ahead_ms: ahead })));
    let far = key.seal_record(ACCOUNT, bypass(), stamp(u64::MAX, u32::MAX, "d_a"), 1, Some("true")).unwrap();
    assert!(matches!(store.apply_verified(&key, ACCOUNT, far, NOW), Err(ApplyError::FutureStamp(_))));
    assert!(store.is_empty());
    // Once the clock has caught up, it's fine.
    assert_eq!(store.apply_verified(&key, ACCOUNT, r, NOW + 1), Ok(true));
}
