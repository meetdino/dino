//! Three devices editing the same settings, offline and online, with changes delivered late, out of
//! order and more than once: once everything has reached everyone, all three hold the same state.

use dino_sync::hlc::{Clock, FutureStamp, Hlc, MAX_SKEW_MS};
use dino_sync::merge::{Store, WriteError};
use dino_sync::record::{Record, RecordId};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use serde_json::{Map, Value, json};

const DEVICES: usize = 3;
const KEYS: [&str; 4] = ["claude.mode", "claude.model", "codex.mode", "allow_bypass"];
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

fn record(id: RecordId, hlc: Hlc, schema: u32, value: Option<Value>) -> Record {
    Record { id, hlc, schema, deleted: value.is_none(), value: value.unwrap_or(Value::Null), seq: None, extra: Map::new() }
}

fn deliver(dev: &mut Device, log: &[Record], mut picks: Vec<usize>, seed: u64) {
    shuffle(&mut picks, seed);
    for i in picks {
        let now = dev.wall.max(0) as u64;
        dev.store.apply_remote(log[i].clone(), now).expect("devices' clocks are minutes apart at most");
        dev.clock.observe(&log[i].hlc, now).expect("devices' clocks are minutes apart at most");
        dev.seen[i] = true;
    }
}

fn state(s: &Store) -> Vec<(RecordId, Value, bool, String)> {
    s.iter().map(|r| (r.id.clone(), r.value.clone(), r.deleted, format!("{:?}", r.hlc))).collect()
}

fn converge(steps: Vec<Step>, skews: [i64; DEVICES]) -> Result<(), TestCaseError> {
    let mut devs: Vec<Device> = (0..DEVICES).map(|i| Device { clock: Clock::new(format!("d{i}")), store: Store::new(), wall: NOW as i64 + skews[i], seen: vec![] }).collect();
    let mut log: Vec<Record> = vec![];
    for s in steps {
        match s {
            Step::Edit(d, k, v, dt) => {
                let dev = &mut devs[d];
                dev.wall += dt;
                let hlc = dev.clock.now(dev.wall.max(0) as u64);
                let r = record(RecordId::new("agents", KEYS[k]), hlc, 1, v.map(|v| json!(format!("v{v}"))));
                dev.store.write_local(r.clone(), 1).expect("a fresh local stamp beats everything this device has seen");
                log.push(r);
                for dv in devs.iter_mut() {
                    dv.seen.resize(log.len(), false);
                }
                devs[d].seen[log.len() - 1] = true;
            }
            Step::Deliver(d, n, seed) => {
                let picks: Vec<usize> = (0..log.len()).filter(|i| !devs[d].seen[*i]).take(n).collect();
                deliver(&mut devs[d], &log, picks, seed);
            }
        }
    }
    // Everything reaches everyone, again, in each device's own order (duplicates are harmless).
    for (i, dev) in devs.iter_mut().enumerate() {
        dev.seen.resize(log.len(), false);
        deliver(dev, &log, (0..log.len()).collect(), i as u64 * 7919 + 1);
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
        converge(steps, skews)?;
    }
}

fn stamp(wall_ms: u64, counter: u32, device: &str) -> Hlc {
    Hlc { wall_ms, counter, device: device.into() }
}

fn bypass() -> RecordId {
    RecordId::new("policies", "allow_bypass")
}

#[test]
fn a_newer_schema_is_kept_and_not_overwritten() {
    let mut store = Store::new();
    let mut newer = Clock::new("new");
    let r = record(RecordId::new("agents", "claude.mode"), newer.now(NOW), 2, Some(json!({"newshape": 1})));
    assert_eq!(store.apply_remote(r.clone(), NOW), Ok(true));
    assert!(!store.readable(&r.id, 1));
    let mut older = Clock::new("old");
    older.observe(&r.hlc, NOW).unwrap();
    let mine = record(r.id.clone(), older.now(NOW + 1000), 1, Some(json!("plan")));
    assert_eq!(store.write_local(mine, 1), Err(WriteError::NewerSchema { stored: 2, ours: 1 }));
    assert_eq!(store.get(&r.id), Some(&r));
}

#[test]
fn a_delete_applies_and_an_old_value_seen_again_loses() {
    let mut store = Store::new();
    let set = record(bypass(), stamp(NOW, 0, "d_a"), 1, Some(json!(false)));
    let delete = record(bypass(), stamp(NOW + 1, 0, "d_a"), 1, None);
    assert!(delete.is_tombstone());
    assert_eq!(store.apply_all([set.clone(), delete.clone()]).len(), 2);
    assert_eq!(store.get(&bypass()), Some(&delete));
    assert!(!store.apply(set), "the high-water mark wins over a replay");
    assert_eq!(store.get(&bypass()), Some(&delete));
}

#[test]
fn stamps_far_in_the_future_are_refused() {
    let mut store = Store::new();
    // It would win every conflict for years.
    let ahead = MAX_SKEW_MS + 1;
    let r = record(bypass(), stamp(NOW + ahead, 0, "d_a"), 1, Some(json!(true)));
    assert_eq!(store.apply_remote(r.clone(), NOW), Err(FutureStamp { ahead_ms: ahead }));
    let far = record(bypass(), stamp(u64::MAX, u32::MAX, "d_a"), 1, Some(json!(true)));
    assert!(store.apply_remote(far, NOW).is_err());
    assert!(store.is_empty());
    // Once the clock has caught up, it's fine.
    assert_eq!(store.apply_remote(r, NOW + 1), Ok(true));
}
