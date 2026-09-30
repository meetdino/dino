//! Three devices editing the same settings, offline and online, with changes delivered late, out of
//! order and more than once: once everything has reached everyone, all three hold the same state.

use dino_sync::hlc::Clock;
use dino_sync::merge::Store;
use dino_sync::record::{Record, RecordId};
use proptest::prelude::*;
use serde_json::Map;

const DEVICES: usize = 3;
const KEYS: [&str; 4] = ["claude.mode", "claude.model", "codex.mode", "allow_bypass"];

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

fn deliver(dev: &mut Device, log: &[Record], mut picks: Vec<usize>, seed: u64) {
    shuffle(&mut picks, seed);
    for i in picks {
        dev.clock.observe(&log[i].hlc, dev.wall.max(0) as u64);
        dev.store.apply(log[i].clone());
        dev.seen[i] = true;
    }
}

fn state(s: &Store) -> Vec<(RecordId, Option<String>, String)> {
    s.iter().map(|r| (r.id.clone(), r.value.clone(), format!("{:?}", r.hlc))).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn any_interleaving_converges(steps in proptest::collection::vec(step(), 1..60), skews in proptest::array::uniform3(-5_000i64..5_000)) {
        let mut devs: Vec<Device> = (0..DEVICES).map(|i| Device { clock: Clock::new(format!("d{i}")), store: Store::new(), wall: 1_700_000_000_000 + skews[i], seen: vec![] }).collect();
        let mut log: Vec<Record> = vec![];
        for s in steps {
            match s {
                Step::Edit(d, k, v, dt) => {
                    let dev = &mut devs[d];
                    dev.wall += dt;
                    let hlc = dev.clock.now(dev.wall.max(0) as u64);
                    let r = Record { id: RecordId::new("agents", KEYS[k]), hlc, schema: 1, value: v.map(|v| format!("v{v}")), seq: None, extra: Map::new() };
                    dev.store.write_local(r.clone(), 1).expect("a fresh local stamp beats everything this device has seen");
                    log.push(r);
                    for dv in devs.iter_mut() { dv.seen.resize(log.len(), false); }
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
        for key in KEYS {
            let latest = log.iter().filter(|r| r.id.key == key).max_by(|a, b| a.hlc.cmp(&b.hlc));
            prop_assert_eq!(devs[0].store.get(&RecordId::new("agents", key)).map(|r| &r.value), latest.map(|r| &r.value));
        }
    }
}

#[test]
fn a_newer_schema_is_kept_and_not_overwritten() {
    let mut store = Store::new();
    let mut newer = Clock::new("new");
    let r = Record { id: RecordId::new("agents", "claude.mode"), hlc: newer.now(1000), schema: 2, value: Some("v4.local.newshape".into()), seq: Some(9), extra: Map::new() };
    assert!(store.apply(r.clone()));
    assert!(!store.readable(&r.id, 1));
    let mut older = Clock::new("old");
    older.observe(&r.hlc, 1000);
    let mine = Record { hlc: older.now(2000), schema: 1, value: Some("v4.local.oldshape".into()), seq: None, ..r.clone() };
    assert_eq!(store.write_local(mine, 1), Err(dino_sync::merge::WriteError::NewerSchema { stored: 2, ours: 1 }));
    assert_eq!(store.get(&r.id), Some(&r));
}
