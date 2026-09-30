//! The official PASETO v4 test vectors (paseto-standard/test-vectors, as shipped with pasetors), for
//! the v4.local tokens dino-sync uses.

use pasetors::Local;
use pasetors::keys::SymmetricKey;
use pasetors::token::UntrustedToken;
use pasetors::version4::{LocalToken, V4};
use serde_json::Value;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn official_v4_local_vectors() {
    let file: Value = serde_json::from_str(include_str!("paseto-v4.json")).unwrap();
    let mut checked = 0;
    for t in file["tests"].as_array().unwrap() {
        let token = t["token"].as_str().unwrap_or("");
        let Some(key) = t["key"].as_str() else { continue };
        if !token.starts_with("v4.local.") && !t["name"].as_str().unwrap().starts_with("4-F") {
            continue;
        }
        let name = t["name"].as_str().unwrap();
        let fail = t["expect-fail"].as_bool().unwrap();
        let footer = t["footer"].as_str().unwrap_or("");
        let implicit = t["implicit-assertion"].as_str().unwrap_or("");
        let opened = SymmetricKey::<V4>::from(&unhex(key)).ok().and_then(|k| {
            let untrusted = UntrustedToken::<Local, V4>::try_from(token).ok()?;
            LocalToken::decrypt(&k, &untrusted, (!footer.is_empty()).then_some(footer.as_bytes()), (!implicit.is_empty()).then_some(implicit.as_bytes())).ok()
        });
        match (fail, opened) {
            (true, None) => {}
            (false, Some(t2)) => assert_eq!(t2.payload(), t["payload"].as_str().unwrap(), "{name}"),
            (true, Some(_)) => panic!("{name} should fail to open"),
            (false, None) => panic!("{name} should open"),
        }
        checked += 1;
    }
    assert!(checked >= 9, "only {checked} v4.local vectors checked");
}
