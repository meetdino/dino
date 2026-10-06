//! End to end against a real Postgres (DINO_TEST_DATABASE_URL, default
//! postgres://dino:dino@127.0.0.1:55432/postgres; see the README), with GitHub, Google and mail faked.

mod common;

use std::time::Duration;

use common::*;
use serde_json::Value;

#[tokio::test]
async fn native_pkce_login_and_the_code_rules() {
    let s = start().await;
    let b = browser();
    assert_eq!(s.email_signin(&b, "pkce@example.com").await, "/account");

    let t = s.native_login(&b, "dino", "Ben's MacBook Pro").await;
    let at = t["access_token"].as_str().unwrap();
    assert!(at.starts_with("dino_at_") && t["refresh_token"].as_str().unwrap().starts_with("dino_rt_"));
    assert_eq!(t["token_type"], "Bearer");
    assert_eq!(t["expires_in"], 900);
    assert_eq!(t["scope"], "account sync");
    assert_eq!(s.me(at).await, 200);
    let me: Value = app().get(s.url("/v1/me")).bearer_auth(at).send().await.unwrap().json().await.unwrap();
    assert_eq!(me["email"], "pkce@example.com");
    let page = b.get(s.url("/account")).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("MacBook Pro") && page.contains("macOS 26"), "the account page lists the device");

    // A wrong verifier, another client, another redirect: all refused, and the code is spent.
    let (code, verifier, redirect) = s.authorize(&b, "dino", "Second Mac").await;
    let bad = app().post(s.url("/oauth/token")).form(&[("grant_type", "authorization_code"), ("client_id", "dino"), ("code", &code), ("redirect_uri", &redirect), ("code_verifier", &format!("{verifier}x"))]).send().await.unwrap();
    assert_eq!(bad.status(), 400);
    let again = app().post(s.url("/oauth/token")).form(&[("grant_type", "authorization_code"), ("client_id", "dino"), ("code", &code), ("redirect_uri", &redirect), ("code_verifier", &verifier)]).send().await.unwrap();
    assert_eq!(again.status(), 400, "a code works once, even after a failed try");

    // A code used twice signs out the device it made.
    let (code, verifier, redirect) = s.authorize(&b, "dino", "Third Mac").await;
    let form = [("grant_type", "authorization_code"), ("client_id", "dino"), ("code", code.as_str()), ("redirect_uri", redirect.as_str()), ("code_verifier", verifier.as_str())];
    let first: Value = app().post(s.url("/oauth/token")).form(&form).send().await.unwrap().json().await.unwrap();
    let at3 = first["access_token"].as_str().unwrap().to_owned();
    assert_eq!(s.me(&at3).await, 200);
    let second = app().post(s.url("/oauth/token")).form(&form).send().await.unwrap();
    assert_eq!(second.status(), 400);
    assert_eq!(s.me(&at3).await, 401, "replaying a code revokes what it issued");
    assert_eq!(s.me(at).await, 200, "other devices are untouched");

    // Cancel sends access_denied back to the app.
    let mut u = url::Url::parse(&s.url("/oauth/authorize")).unwrap();
    u.query_pairs_mut().extend_pairs([("response_type", "code"), ("client_id", "dino"), ("redirect_uri", "http://127.0.0.1:1/callback"), ("state", "s"), ("code_challenge", "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"), ("code_challenge_method", "S256")]);
    b.get(u.as_str()).send().await.unwrap();
    let page = b.get(s.url("/oauth/authorize/confirm")).send().await.unwrap().text().await.unwrap();
    let r = b.post(s.url("/oauth/authorize/decide")).form(&[("csrf", csrf(&page).as_str()), ("decision", "deny")]).send().await.unwrap();
    assert!(location(&r).contains("error=access_denied"));

    // Nothing secret reaches the logs: tokens, codes, the emailed code.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let logs = logs();
    assert!(!logs.is_empty());
    for secret in [at, t["refresh_token"].as_str().unwrap(), &at3, &code, &verifier, &s.mailed_code("pkce@example.com")] {
        assert!(!logs.contains(secret), "a secret was logged");
    }
    assert!(logs.contains("/oauth/token"), "routes are logged");
}

#[tokio::test]
async fn authorize_refuses_bad_clients_and_redirects() {
    let s = start().await;
    let b = browser();
    let get = |q: &str| {
        let b = b.clone();
        let u = s.url(&format!("/oauth/authorize?{q}"));
        async move { b.get(u).send().await.unwrap() }
    };
    let ch = "code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256";
    // An unknown client or a redirect it didn't register: a page, never a redirect.
    for q in [
        format!("response_type=code&client_id=evil&redirect_uri=http://127.0.0.1:1/callback&{ch}"),
        format!("response_type=code&client_id=dino&redirect_uri=https://evil.example/callback&{ch}"),
        format!("response_type=code&client_id=dino&redirect_uri=http://localhost:1/callback&{ch}"),
    ] {
        let r = get(&q).await;
        assert_eq!(r.status(), 200, "{q}");
        assert!(location(&r).is_empty());
    }
    // PKCE is required, and only S256.
    let r = get("response_type=code&client_id=dino&redirect_uri=http://127.0.0.1:1/callback&code_challenge=abc&code_challenge_method=plain").await;
    assert!(location(&r).contains("error=invalid_request"));
    let r = get("response_type=token&client_id=dino&redirect_uri=http://127.0.0.1:1/callback").await;
    assert!(location(&r).contains("error=unsupported_response_type"));
    let r = get(&format!("response_type=code&client_id=dino&redirect_uri=http://127.0.0.1:1/callback&scope=admin&{ch}")).await;
    assert!(location(&r).contains("error=invalid_scope"));
    // Not signed in: sign in first, then back to confirm.
    let r = get(&format!("response_type=code&client_id=dino&redirect_uri=http://127.0.0.1:1/callback&{ch}")).await;
    assert_eq!(location(&r), "/signin");
    assert_eq!(s.email_signin(&b, "later@example.com").await, "/oauth/authorize/confirm");

    // Metadata.
    let m: Value = app().get(s.url("/.well-known/oauth-authorization-server")).send().await.unwrap().json().await.unwrap();
    assert_eq!(m["issuer"], s.base);
    assert_eq!(m["code_challenge_methods_supported"], serde_json::json!(["S256"]));
    assert_eq!(m["device_authorization_endpoint"], s.url("/oauth/device_authorization"));
}

#[tokio::test]
async fn refresh_rotation_grace_and_reuse_detection() {
    let s = start().await;
    let b = browser();
    s.email_signin(&b, "rotate@example.com").await;
    let t = s.native_login(&b, "dino", "Rotating Mac").await;
    let rt1 = t["refresh_token"].as_str().unwrap().to_owned();
    let other = s.native_login(&b, "dino", "Other Mac").await;

    let r2: Value = s.refresh("dino", &rt1).await.json().await.unwrap();
    let rt2 = r2["refresh_token"].as_str().unwrap().to_owned();
    assert_ne!(rt1, rt2, "every refresh rotates");
    assert_eq!(s.me(r2["access_token"].as_str().unwrap()).await, 200);

    // Lost the answer and asked again at once: same replacement, still signed in.
    let again: Value = s.refresh("dino", &rt1).await.json().await.unwrap();
    assert_eq!(again["refresh_token"], rt2.as_str());

    // Two refreshes racing on one token both get the same next token.
    let (a, b2) = futures::join!(s.refresh("dino", &rt2), s.refresh("dino", &rt2));
    let (a, b2): (Value, Value) = (a.json().await.unwrap(), b2.json().await.unwrap());
    assert_eq!(a["refresh_token"], b2["refresh_token"]);
    let rt3 = a["refresh_token"].as_str().unwrap().to_owned();
    let at3 = a["access_token"].as_str().unwrap().to_owned();

    // Another client can't use it.
    assert_eq!(s.refresh("dino-harness", &rt3).await.status(), 400);

    // rt1 comes back after the grace period: someone else has the family.
    sqlx::query("UPDATE refresh_tokens SET rotated_at = now() - interval '5 minutes' WHERE hash = $1").bind(sha256(&rt1)).execute(&s.state.db).await.unwrap();
    let reuse = s.refresh("dino", &rt1).await;
    assert_eq!(reuse.status(), 400);
    let body: Value = reuse.json().await.unwrap();
    assert_eq!(body["error"], "invalid_grant");
    assert_eq!(s.refresh("dino", &rt3).await.status(), 400, "the whole family is revoked");
    assert_eq!(s.me(&at3).await, 401, "and its access tokens");
    let reason: (Option<String>,) = sqlx::query_as("SELECT revoke_reason FROM devices WHERE name = 'Rotating Mac'").fetch_one(&s.state.db).await.unwrap();
    assert_eq!(reason.0.as_deref(), Some("refresh_token_reuse"));
    assert_eq!(s.me(other["access_token"].as_str().unwrap()).await, 200, "the other device isn't touched");

    // RFC 7009 revocation of a refresh token signs that device out.
    let ort = other["refresh_token"].as_str().unwrap();
    assert_eq!(app().post(s.url("/oauth/revoke")).form(&[("token", ort), ("client_id", "dino")]).send().await.unwrap().status(), 200);
    assert_eq!(s.me(other["access_token"].as_str().unwrap()).await, 401);
}

#[tokio::test]
async fn device_flow_with_consent_and_fresh_sign_in() {
    let s = start().await;
    let form = [("client_id", "dino"), ("scope", "account"), ("device_name", "build-box"), ("device_os", "Linux"), ("dino_version", "0.1.0")];
    let d: Value = app().post(s.url("/oauth/device_authorization")).form(&form).send().await.unwrap().json().await.unwrap();
    let device_code = d["device_code"].as_str().unwrap().to_owned();
    let user_code = d["user_code"].as_str().unwrap().to_owned();
    assert_eq!(user_code.len(), 9);
    assert_eq!(d["verification_uri"], s.url("/device"));
    assert_eq!(d["interval"], 5);
    let poll = || app().post(s.url("/oauth/token")).form(&[("grant_type", "urn:ietf:params:oauth:grant-type:device_code"), ("client_id", "dino"), ("device_code", device_code.as_str())]).send();
    let e: Value = poll().await.unwrap().json().await.unwrap();
    assert_eq!(e["error"], "authorization_pending");
    let e: Value = poll().await.unwrap().json().await.unwrap();
    assert_eq!(e["error"], "slow_down", "polling faster than the interval");

    // Someone who isn't signed in is sent to sign in, then back to the code.
    let b = browser();
    let r = b.get(s.url(&format!("/device?user_code={}", user_code.to_lowercase()))).send().await.unwrap();
    assert_eq!(location(&r), "/signin");
    assert_eq!(s.email_signin(&b, "device@example.com").await, format!("/device?user_code={user_code}"));
    let page = b.get(s.url(&format!("/device?user_code={user_code}"))).send().await.unwrap().text().await.unwrap();
    assert!(page.contains(&user_code), "prefilled");
    let consent = b.post(s.url("/device")).form(&[("csrf", csrf(&page).as_str()), ("user_code", user_code.as_str())]).send().await.unwrap().text().await.unwrap();
    assert!(consent.contains("build-box") && consent.contains("Linux") && consent.contains(&user_code), "names the device and shows the code");
    assert!(consent.contains("Approve"));

    // A sign-in more than 10 minutes old can't approve.
    sqlx::query("UPDATE web_sessions SET authed_at = now() - interval '1 hour' WHERE account_id IS NOT NULL").execute(&s.state.db).await.unwrap();
    let r = b.post(s.url("/device/decide")).form(&[("csrf", csrf(&consent).as_str()), ("user_code", user_code.as_str()), ("decision", "approve")]).send().await.unwrap();
    assert_eq!(r.status(), 403);
    sqlx::query("UPDATE web_sessions SET authed_at = now() WHERE account_id IS NOT NULL").execute(&s.state.db).await.unwrap();
    let r = b.post(s.url("/device/decide")).form(&[("csrf", csrf(&consent).as_str()), ("user_code", user_code.as_str()), ("decision", "approve")]).send().await.unwrap();
    assert!(r.text().await.unwrap().contains("Device connected"));

    // After slow_down the interval is 10 s.
    tokio::time::sleep(Duration::from_millis(10_300)).await;
    let t: Value = poll().await.unwrap().json().await.unwrap();
    let at = t["access_token"].as_str().expect("tokens after approval");
    assert_eq!(s.me(at).await, 200);
    tokio::time::sleep(Duration::from_millis(10_300)).await;
    let used: Value = poll().await.unwrap().json().await.unwrap();
    assert_eq!(used["error"], "invalid_grant", "single use");

    // Deny works too, and a wrong code is just "not found".
    let d: Value = app().post(s.url("/oauth/device_authorization")).form(&form).send().await.unwrap().json().await.unwrap();
    let code2 = d["user_code"].as_str().unwrap().to_owned();
    let page = b.get(s.url("/device")).send().await.unwrap().text().await.unwrap();
    let nf = b.post(s.url("/device")).form(&[("csrf", csrf(&page).as_str()), ("user_code", "BBBB-BBBB")]).send().await.unwrap().text().await.unwrap();
    assert!(nf.contains("Code not found"));
    let consent = b.post(s.url("/device")).form(&[("csrf", csrf(&page).as_str()), ("user_code", code2.as_str())]).send().await.unwrap().text().await.unwrap();
    b.post(s.url("/device/decide")).form(&[("csrf", csrf(&consent).as_str()), ("user_code", code2.as_str()), ("decision", "deny")]).send().await.unwrap();
    let e: Value = app().post(s.url("/oauth/token")).form(&[("grant_type", "urn:ietf:params:oauth:grant-type:device_code"), ("client_id", "dino"), ("device_code", d["device_code"].as_str().unwrap())]).send().await.unwrap().json().await.unwrap();
    assert_eq!(e["error"], "access_denied");
}

#[tokio::test]
async fn github_and_google_link_to_one_account() {
    let s = start().await;
    let b = browser();
    let signin = |provider: &'static str, code: &'static str| {
        let b = b.clone();
        let s = &s;
        async move {
            let r = b.get(s.url(&format!("/signin/{provider}"))).send().await.unwrap();
            let to = url::Url::parse(&location(&r)).unwrap();
            let q: std::collections::HashMap<_, _> = to.query_pairs().into_owned().collect();
            assert_eq!(q["code_challenge_method"], "S256");
            assert_eq!(q["redirect_uri"], s.url(&format!("/signin/{provider}/callback")));
            // A forged state is refused.
            let bad = b.get(s.url(&format!("/signin/{provider}/callback?code={code}&state=forged"))).send().await.unwrap().text().await.unwrap();
            assert!(bad.contains("didn't finish"));
            let r = b.get(s.url(&format!("/signin/{provider}"))).send().await.unwrap();
            let q: std::collections::HashMap<_, _> = url::Url::parse(&location(&r)).unwrap().query_pairs().into_owned().collect();
            let r = b.get(s.url(&format!("/signin/{provider}/callback?code={code}&state={}", q["state"]))).send().await.unwrap();
            assert_eq!(location(&r), "/account", "{provider} signs in");
        }
    };
    signin("github", "GHCODE").await;
    let first: (uuid::Uuid,) = sqlx::query_as("SELECT account_id FROM identities WHERE provider = 'github'").fetch_one(&s.state.db).await.unwrap();
    signin("google", "GCODE").await;
    let second: (uuid::Uuid,) = sqlx::query_as("SELECT account_id FROM identities WHERE provider = 'google'").fetch_one(&s.state.db).await.unwrap();
    assert_eq!(first.0, second.0, "same verified address, same account");
    let page = b.get(s.url("/account")).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("gh-and-google@example.com") && page.contains("github, google"));
}

#[tokio::test]
async fn sign_out_everywhere_revoke_delete_and_erase() {
    let s = start().await;
    let b = browser();
    s.email_signin(&b, "leaving@example.com").await;
    let one = s.native_login(&b, "dino", "Mac one").await;
    let two = s.native_login(&b, "dino", "Mac two").await;
    let at1 = one["access_token"].as_str().unwrap();

    // A device's token reaches only `/v1/me` and sync: the rest is the account page's.
    for (method, path) in [("GET", "/v1/devices"), ("POST", "/v1/signout-everywhere"), ("DELETE", "/v1/account"), ("GET", "/v1/export")] {
        let r = app().request(method.parse().unwrap(), s.url(path)).bearer_auth(at1).send().await.unwrap();
        assert_eq!(r.status(), 404, "{method} {path}");
    }
    assert_eq!(s.me(at1).await, 200);

    let page = b.get(s.url("/account")).send().await.unwrap().text().await.unwrap();
    let r = b.post(s.url("/account/signout-everywhere")).form(&[("csrf", csrf(&page).as_str())]).send().await.unwrap();
    assert_eq!(location(&r), "/signin");
    assert_eq!(s.me(at1).await, 401);
    assert_eq!(s.me(two["access_token"].as_str().unwrap()).await, 401);
    assert_eq!(s.refresh("dino", two["refresh_token"].as_str().unwrap()).await.status(), 400);
    // The browser session went too.
    let r = b.get(s.url("/account")).send().await.unwrap();
    assert_eq!(location(&r), "/signin");

    // One device signed out from the page; the others stay.
    s.email_signin(&b, "leaving@example.com").await;
    let three = s.native_login(&b, "dino", "Mac three").await;
    let four = s.native_login(&b, "dino", "Mac four").await;
    let (at3, at4) = (three["access_token"].as_str().unwrap(), four["access_token"].as_str().unwrap());
    let page = b.get(s.url("/account")).send().await.unwrap().text().await.unwrap();
    let r = b.post(s.url(&format!("/account/devices/{}/revoke", three["device_id"].as_str().unwrap()))).form(&[("csrf", csrf(&page).as_str())]).send().await.unwrap();
    assert_eq!(location(&r), "/account");
    assert_eq!(s.me(at3).await, 401);
    assert_eq!(s.me(at4).await, 200, "the other device is still signed in");

    // Export has the account and every device.
    let ex: Value = b.get(s.url("/account/export")).send().await.unwrap().json().await.unwrap();
    assert_eq!(ex["email"], "leaving@example.com");
    assert_eq!(ex["devices"].as_array().unwrap().len(), 4);

    // Delete: only with the address typed, then everything revoked now, sign-in refused, rows
    // erased 30 days later.
    let page = b.get(s.url("/account")).send().await.unwrap().text().await.unwrap();
    let r = b.post(s.url("/account/delete")).form(&[("csrf", csrf(&page).as_str()), ("confirm", "someone@example.com")]).send().await.unwrap();
    assert!(r.text().await.unwrap().contains("Not deleted"));
    assert_eq!(s.me(at4).await, 200);
    let r = b.post(s.url("/account/delete")).form(&[("csrf", csrf(&page).as_str()), ("confirm", "Leaving@Example.com")]).send().await.unwrap();
    assert!(r.text().await.unwrap().contains("Account deleted"));
    assert_eq!(s.me(at4).await, 401);
    let b2 = browser();
    let page = b2.get(s.url("/signin")).send().await.unwrap().text().await.unwrap();
    b2.post(s.url("/signin/email")).form(&[("csrf", csrf(&page).as_str()), ("email", "leaving@example.com")]).send().await.unwrap();
    let page = b2.get(s.url("/signin/email/code")).send().await.unwrap().text().await.unwrap();
    let r = b2.post(s.url("/signin/email/code")).form(&[("csrf", csrf(&page).as_str()), ("code", s.mailed_code("leaving@example.com").as_str())]).send().await.unwrap();
    assert_eq!(r.status(), 403, "a deleted account can't sign in");
    sqlx::query("UPDATE accounts SET deleted_at = now() - interval '31 days'").execute(&s.state.db).await.unwrap();
    dino_cloud::jobs::cleanup(&s.state).await.unwrap();
    let left: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM accounts), (SELECT count(*) FROM devices), (SELECT count(*) FROM identities)").fetch_one(&s.state.db).await.unwrap();
    assert_eq!(left, (0, 0, 0), "erased");
}

#[tokio::test]
async fn csrf_origin_and_email_code_limits() {
    let s = start().await;
    let b = browser();
    let page = b.get(s.url("/signin")).send().await.unwrap().text().await.unwrap();
    let token = csrf(&page);
    let r = b.post(s.url("/signin/email")).form(&[("csrf", "wrong"), ("email", "x@example.com")]).send().await.unwrap();
    assert_eq!(r.status(), 403, "no valid CSRF token");
    let r = b.post(s.url("/signin/email")).header("origin", "https://evil.example").form(&[("csrf", token.as_str()), ("email", "x@example.com")]).send().await.unwrap();
    assert_eq!(r.status(), 403, "cross-site form");
    // Wrong codes: five tries, then even the right one is refused.
    b.post(s.url("/signin/email")).form(&[("csrf", token.as_str()), ("email", "tries@example.com")]).send().await.unwrap();
    let page = b.get(s.url("/signin/email/code")).send().await.unwrap().text().await.unwrap();
    let good = s.mailed_code("tries@example.com");
    let bad = if good == "000000" { "111111" } else { "000000" };
    for _ in 0..5 {
        let t = b.post(s.url("/signin/email/code")).form(&[("csrf", csrf(&page).as_str()), ("code", bad)]).send().await.unwrap().text().await.unwrap();
        assert!(t.contains("Code not accepted"));
    }
    let t = b.post(s.url("/signin/email/code")).form(&[("csrf", csrf(&page).as_str()), ("code", good.as_str())]).send().await.unwrap().text().await.unwrap();
    assert!(t.contains("Code not accepted"), "locked after five tries");
    // Security headers.
    let r = b.get(s.url("/signin")).send().await.unwrap();
    let csp = r.headers().get("content-security-policy").unwrap().to_str().unwrap();
    assert!(csp.contains("frame-ancestors 'none'") && csp.contains("default-src 'none'"));
    assert_eq!(r.headers().get("x-frame-options").unwrap(), "DENY");
    let set_cookie = browser().get(s.url("/signin")).send().await.unwrap().headers().get("set-cookie").unwrap().to_str().unwrap().to_owned();
    assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Lax"));
}

#[tokio::test]
async fn rate_limits_trip() {
    let s = start().await;
    let mut limited = 0;
    for _ in 0..40 {
        let r = app().post(s.url("/oauth/token")).form(&[("grant_type", "authorization_code"), ("client_id", "dino"), ("code", "nope"), ("redirect_uri", "http://127.0.0.1:1/callback"), ("code_verifier", "x")]).send().await.unwrap();
        if r.status() == 429 {
            assert!(r.headers().get("retry-after").is_some());
            limited += 1;
        }
    }
    assert!(limited >= 15, "the auth limit (burst 20) trips: {limited}");
    // The account API has its own limit per account.
    let b = browser();
    // A fresh address for the limiter bucket isn't possible from one IP, so wait out the burst.
    tokio::time::sleep(Duration::from_secs(21)).await;
    s.email_signin(&b, "busy@example.com").await;
    let t = s.native_login(&b, "dino", "Busy Mac").await;
    let at = t["access_token"].as_str().unwrap();
    let mut over = 0;
    for _ in 0..90 {
        if s.me(at).await == 429 {
            over += 1;
        }
    }
    assert!(over > 0, "the per-account limit trips");
}

#[tokio::test]
async fn harness_tokens_have_their_own_audience() {
    let s = start().await;
    let b = browser();
    s.email_signin(&b, "harness@example.com").await;
    let t = s.native_login(&b, "dino-harness", "harness on Mac").await;
    let at = t["access_token"].as_str().unwrap();
    assert_eq!(s.me(at).await, 401, "harness tokens don't open dino's account API");
    // dino tokens reach sync; a fresh account has nothing yet.
    let d = s.native_login(&b, "dino", "Mac").await;
    let r: Value = app().get(s.url("/v1/sync")).bearer_auth(d["access_token"].as_str().unwrap()).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["seq"], 0);
    assert_eq!(app().get(s.url("/v1/sync")).bearer_auth(at).send().await.unwrap().status(), 401, "harness tokens don't");
    assert_eq!(app().get(s.url("/readyz")).send().await.unwrap().status(), 200);
}

fn link_poll(s: &Server, device_code: &str) -> impl std::future::Future<Output = Value> {
    let req = app().post(s.url("/oauth/token")).form(&[("grant_type", "urn:ietf:params:oauth:grant-type:device_code"), ("client_id", "dino"), ("device_code", device_code)]).send();
    async move { req.await.unwrap().json().await.unwrap() }
}

async fn ask_for_link(s: &Server, email: &str) -> reqwest::Response {
    let form = [("client_id", "dino"), ("scope", "account sync"), ("device_name", "Ben's MacBook Air"), ("device_os", "macOS 26"), ("dino_version", "0.1.0"), ("email", email)];
    app().post(s.url("/oauth/device_authorization")).form(&form).send().await.unwrap()
}

#[tokio::test]
async fn a_mailed_link_signs_the_device_in_once() {
    let s = start().await;
    // The answer looks the same whether or not the address has an account.
    s.email_signin(&browser(), "known@example.com").await;
    let known: Value = ask_for_link(&s, "Known@Example.com").await.json().await.unwrap();
    let r = ask_for_link(&s, "new@example.com").await;
    assert_eq!(r.status(), 200);
    let d: Value = r.json().await.unwrap();
    let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert_eq!(keys(&known), keys(&d), "no account enumeration");
    assert_eq!((d["interval"].as_i64(), d["expires_in"].as_i64()), (Some(2), Some(900)));
    let device_code = d["device_code"].as_str().unwrap().to_owned();
    assert_eq!(link_poll(&s, &device_code).await["error"], "authorization_pending");

    // Opening the link (as a mail scanner would) uses nothing up.
    let link = s.mailed_link("new@example.com");
    assert!(std::fs::read_to_string(&s.mail).unwrap().contains(&s.url(&link)), "the link is on this server");
    let page = browser().get(s.url(&link)).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("Ben&#39;s MacBook Air") || page.contains("Ben's MacBook Air"), "names the device");
    assert!(page.contains("new@example.com"));
    tokio::time::sleep(Duration::from_millis(2100)).await;
    assert_eq!(link_poll(&s, &device_code).await["error"], "authorization_pending");

    // The button signs the device in.
    let done = browser().post(s.url(&link)).send().await.unwrap().text().await.unwrap();
    assert!(done.contains("signed in on Ben"), "{done}");
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let t = link_poll(&s, &device_code).await;
    let at = t["access_token"].as_str().expect("tokens once the link was opened");
    let me: Value = app().get(s.url("/v1/me")).bearer_auth(at).send().await.unwrap().json().await.unwrap();
    assert_eq!(me["email"], "new@example.com");
    assert_eq!(me["identities"][0]["provider"], "email");

    // Used once: opening it again, or posting again, does nothing.
    let again = browser().post(s.url(&link)).send().await.unwrap().text().await.unwrap();
    assert!(again.contains("expired"));
    assert!(browser().get(s.url(&link)).send().await.unwrap().text().await.unwrap().contains("expired"));
    // A link past its 15 minutes is refused too, and a made-up one.
    ask_for_link(&s, "late@example.com").await;
    sqlx::query("UPDATE email_links SET expires_at = now() - interval '1 second' WHERE email = 'late@example.com'").execute(&s.state.db).await.unwrap();
    let late = browser().post(s.url(&s.mailed_link("late@example.com"))).send().await.unwrap().text().await.unwrap();
    assert!(late.contains("expired"));
    assert!(browser().post(s.url("/login/dino_ml_made-up")).send().await.unwrap().text().await.unwrap().contains("expired"));
    // The known address signs in to its existing account.
    let link = s.mailed_link("known@example.com");
    browser().post(s.url(&link)).send().await.unwrap();
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let t = link_poll(&s, known["device_code"].as_str().unwrap()).await;
    let me: Value = app().get(s.url("/v1/me")).bearer_auth(t["access_token"].as_str().unwrap()).send().await.unwrap().json().await.unwrap();
    assert_eq!(me["email"], "known@example.com");
    let accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts WHERE email = 'known@example.com'").fetch_one(&s.state.db).await.unwrap();
    assert_eq!(accounts, 1);
}

#[tokio::test]
async fn sign_in_links_are_limited_per_address() {
    let s = start().await;
    for _ in 0..5 {
        assert_eq!(ask_for_link(&s, "busy@example.com").await.status(), 200);
    }
    let r = ask_for_link(&s, "busy@example.com").await;
    assert_eq!(r.status(), 429);
    assert!(r.headers().get("retry-after").is_some());
    assert_eq!(ask_for_link(&s, "not an address").await.status(), 400);
}

#[tokio::test]
async fn sign_in_with_github_is_one_click_and_joins_the_email_account() {
    let s = start().await;
    // The address already has an account, from an emailed code.
    s.email_signin(&browser(), "gh-and-google@example.com").await;
    let existing: (uuid::Uuid,) = sqlx::query_as("SELECT id FROM accounts WHERE email = 'gh-and-google@example.com'").fetch_one(&s.state.db).await.unwrap();

    let b = browser();
    let verifier = "v".repeat(43);
    let challenge = {
        use base64::Engine;
        use sha2::Digest;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()))
    };
    let redirect = "http://127.0.0.1:53682/callback";
    let mut u = url::Url::parse(&s.url("/oauth/authorize")).unwrap();
    u.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", "dino")
        .append_pair("redirect_uri", redirect)
        .append_pair("state", "st-gh")
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("scope", "account sync")
        .append_pair("device_name", "Mac GH")
        .append_pair("provider", "github");
    // Straight to GitHub: no dino sign-in page.
    let r = b.get(u.as_str()).send().await.unwrap();
    assert_eq!(location(&r), "/signin/github");
    let r = b.get(s.url("/signin/github")).send().await.unwrap();
    let to = url::Url::parse(&location(&r)).unwrap();
    assert!(to.path().ends_with("/gh/authorize"), "GitHub's authorize page");
    let q: std::collections::HashMap<_, _> = to.query_pairs().into_owned().collect();
    // Back from GitHub: straight back to the app, no confirm page.
    let r = b.get(s.url(&format!("/signin/github/callback?code=GHCODE&state={}", q["state"]))).send().await.unwrap();
    let back = url::Url::parse(&location(&r)).unwrap();
    assert!(back.as_str().starts_with(redirect), "back to the app: {back}");
    let q: std::collections::HashMap<_, _> = back.query_pairs().into_owned().collect();
    assert_eq!(q["state"], "st-gh");
    let t: Value = app()
        .post(s.url("/oauth/token"))
        .form(&[("grant_type", "authorization_code"), ("client_id", "dino"), ("code", q["code"].as_str()), ("redirect_uri", redirect), ("code_verifier", verifier.as_str())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let me: Value = app().get(s.url("/v1/me")).bearer_auth(t["access_token"].as_str().unwrap()).send().await.unwrap().json().await.unwrap();
    assert_eq!(me["account_id"], existing.0.to_string(), "GitHub joined the account with the same verified address");
    let providers: Vec<&str> = me["identities"].as_array().unwrap().iter().map(|i| i["provider"].as_str().unwrap()).collect();
    assert_eq!(providers, ["email", "github"]);
    let subject: (String,) = sqlx::query_as("SELECT subject FROM identities WHERE provider = 'github'").fetch_one(&s.state.db).await.unwrap();
    assert_eq!(subject.0, "4242", "keyed on GitHub's user id");

    // Without `provider`, a signed-in browser still gets the confirm page.
    let r = b.get(u.as_str().replace("&provider=github", "")).send().await.unwrap();
    assert_eq!(location(&r), "/oauth/authorize/confirm");
}

#[tokio::test]
async fn a_database_out_of_reach_is_a_503_not_a_crash() {
    let s = start().await;
    assert_eq!(app().get(s.url("/readyz")).send().await.unwrap().status(), 200);
    // Every connection taken: requests wait their turn, then hear "try again".
    let mut held = Vec::new();
    while let Ok(c) = s.state.db.acquire().await {
        held.push(c);
    }
    let r = app().get(s.url("/v1/me")).bearer_auth("dino_at_whatever").send().await.unwrap();
    assert_eq!(r.status(), 503);
    assert_eq!(r.headers().get("retry-after").unwrap(), "5");
    drop(held);
    assert_eq!(app().get(s.url("/readyz")).send().await.unwrap().status(), 200, "and it's fine after");
}
