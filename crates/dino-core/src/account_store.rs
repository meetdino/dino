//! Where your other Claude accounts' tokens (each one `claude setup-token` printed) are kept.
//!
//! On macOS: the login Keychain, one generic password per account, service
//! [`service`] ("dino Claude account"), account "Account <n>". dinod adds them, and the item's
//! access list then trusts dinod's own signed binary (its designated requirement: for a release,
//! dino's identifier and Developer ID team), so a dino update reads them without asking, and other
//! apps can't without the user's say-so. dino never lets the Keychain ask: with no say-so, an item
//! reads as unreadable (see [`Accounts::unreadable`]). Read once and kept in memory.
//!
//! On Linux (no Keychain; Secret Service needs a desktop session most hosts dinod runs on don't
//! have): dino's mode-600 key file, as `CLAUDE_ACCOUNT_<n>`. Also where tokens lived on macOS
//! before, which [`migrate`] moves into the Keychain.
//!
//! For tests: `DINO_CLAUDE_ACCOUNT_STORE=file` keeps them in the key file; `DINO_KEYCHAIN` names a
//! keychain file to use instead of the login one, `DINO_KEYCHAIN_SERVICE` a service name. A dino
//! with a `DINO_HOME` of its own gets a service name of its own too.

use crate::claude_token::{self as token, account_key};

/// The accounts as kept: each number's token, and the numbers whose token is there but can't be
/// read (a Keychain item a dino signed differently saved). Their numbers stay taken.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Accounts {
    pub kept: Vec<(u32, String)>,
    pub unreadable: Vec<u32>,
}

impl Accounts {
    pub fn taken(&self, n: u32) -> bool {
        self.kept.iter().any(|(m, _)| *m == n) || self.unreadable.contains(&n)
    }
}

/// The accounts, in order. Never fails: what can't be read now is left out (see [`problem`]).
pub fn load() -> Accounts {
    imp::load()
}

/// Keep (`Some`) or forget (`None`) each account's token.
pub fn set(changes: &[(u32, Option<&str>)]) -> anyhow::Result<()> {
    imp::set(changes)
}

/// What's wrong with reading them now, if anything (the Keychain locked, say).
pub fn problem() -> Option<String> {
    imp::problem()
}

/// `keys` (dino's key store) with each account's token added as `CLAUDE_ACCOUNT_<n>`, as the
/// proxy takes them; held in memory only.
pub fn with_accounts(mut keys: std::collections::HashMap<String, String>) -> std::collections::HashMap<String, String> {
    keys.retain(|k, _| !k.starts_with(token::ACCOUNT_PREFIX));
    for (n, t) in load().kept {
        keys.insert(account_key(n), t);
    }
    keys
}

/// Move tokens the key file holds into the Keychain, then out of the file: each written and read
/// back before the file lets go of it, so stopping halfway loses nothing, and running it again
/// finishes the job. One already in the Keychain isn't added twice; a number taken there by
/// another token gets the first free one. The numbers moved.
pub fn migrate() -> anyhow::Result<Vec<u32>> {
    imp::migrate()
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn file_accounts() -> Vec<(u32, String)> {
    let keys: std::collections::HashMap<String, String> = crate::settings::stored().into_iter().collect();
    token::accounts(&keys)
}

fn file_set(changes: &[(u32, Option<&str>)]) -> anyhow::Result<()> {
    let names: Vec<String> = changes.iter().map(|(n, _)| account_key(*n)).collect();
    let changes: Vec<(&str, Option<&str>)> = names.iter().map(String::as_str).zip(changes.iter().map(|(_, t)| *t)).collect();
    crate::settings::set_keys(&changes)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn in_file() -> bool {
    !cfg!(target_os = "macos") || std::env::var("DINO_CLAUDE_ACCOUNT_STORE").is_ok_and(|v| v == "file")
}

/// The Keychain service the tokens are kept under.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn service() -> String {
    if let Ok(s) = std::env::var("DINO_KEYCHAIN_SERVICE")
        && !s.is_empty()
    {
        return s;
    }
    match std::env::var_os("DINO_HOME") {
        Some(home) => format!("dino Claude account ({})", std::path::Path::new(&home).display()),
        None => "dino Claude account".into(),
    }
}

/// Which number a Keychain item's account name ("Account 3") is.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn number(account: &str) -> Option<u32> {
    account.strip_prefix("Account ")?.parse().ok().filter(|n| *n >= 2)
}

/// Where `file` tokens go in a Keychain that holds `kc`: those already there nowhere; the others
/// at their own number, or the first one free when another token has it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn placement(file: &[(u32, String)], kc: &Accounts) -> Vec<(u32, String)> {
    let mut taken: Vec<u32> = kc.kept.iter().map(|(n, _)| *n).chain(kc.unreadable.iter().copied()).collect();
    let mut out = vec![];
    for (n, t) in file {
        if kc.kept.iter().any(|(_, k)| k == t) {
            continue;
        }
        let at = if taken.contains(n) { (2..).find(|m| !taken.contains(m) && !file.iter().any(|(f, _)| f == m)).unwrap_or(*n) } else { *n };
        taken.push(at);
        out.push((at, t.clone()));
    }
    out
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub fn load() -> Accounts {
        Accounts { kept: file_accounts(), unreadable: vec![] }
    }
    pub fn set(changes: &[(u32, Option<&str>)]) -> anyhow::Result<()> {
        file_set(changes)
    }
    pub fn problem() -> Option<String> {
        None
    }
    pub fn migrate() -> anyhow::Result<Vec<u32>> {
        Ok(vec![])
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit, SearchResult};
    use security_framework::os::macos::keychain::SecKeychain;
    use security_framework::os::macos::passwords::find_generic_password;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// What was read last, and when reading last failed (not tried again for a while).
    struct State {
        read: Option<Accounts>,
        problem: Option<String>,
        failed_at: Option<Instant>,
    }

    /// Also the one lock every Keychain call is made under: turning the Keychain's prompts off is
    /// for the whole process.
    static STATE: Mutex<State> = Mutex::new(State { read: None, problem: None, failed_at: None });
    const RETRY: Duration = Duration::from_secs(30);

    fn keychain() -> security_framework::base::Result<SecKeychain> {
        match std::env::var_os("DINO_KEYCHAIN") {
            Some(p) => SecKeychain::open(p),
            None => SecKeychain::default(),
        }
    }

    fn said(e: &security_framework::base::Error) -> String {
        match e.code() {
            -25308 => "the Keychain is locked, or didn't let dino in without asking".into(),
            -25293 => "the Keychain didn't let dino in".into(),
            c => format!("the Keychain said {c}: {}", e.message().unwrap_or_default()),
        }
    }

    /// Every account the Keychain holds for dino, with nothing asked of the user.
    fn read(kc: &SecKeychain) -> Result<Accounts, String> {
        let _quiet = SecKeychain::disable_user_interaction().map_err(|e| said(&e))?;
        let service = service();
        let found = ItemSearchOptions::new().class(ItemClass::generic_password()).keychains(std::slice::from_ref(kc)).service(&service).load_attributes(true).limit(Limit::All).search();
        let found = match found {
            Ok(f) => f,
            Err(e) if e.code() == -25300 => vec![],
            Err(e) => return Err(said(&e)),
        };
        let mut out = Accounts::default();
        for r in found {
            let SearchResult::Dict(_) = &r else { continue };
            let Some(n) = r.simplify_dict().and_then(|d| d.get("acct").cloned()).as_deref().and_then(number) else { continue };
            match find_generic_password(Some(std::slice::from_ref(kc)), &service, &format!("Account {n}")) {
                Ok((pw, _)) => match std::str::from_utf8(&pw).ok().map(str::trim).filter(|t| token::valid(t)) {
                    Some(t) => out.kept.push((n, t.to_string())),
                    None => out.unreadable.push(n),
                },
                Err(e) if matches!(e.code(), -25308 | -25293 | -128) => out.unreadable.push(n),
                Err(e) => return Err(said(&e)),
            }
        }
        out.kept.sort();
        out.unreadable.sort();
        Ok(out)
    }

    fn refresh(st: &mut State) {
        if st.read.is_some() || st.failed_at.is_some_and(|t| t.elapsed() < RETRY) {
            return;
        }
        match keychain().map_err(|e| said(&e)).and_then(|kc| read(&kc)) {
            Ok(a) => {
                st.read = Some(a);
                st.problem = None;
                st.failed_at = None;
            }
            Err(e) => {
                st.problem = Some(e);
                st.failed_at = Some(Instant::now());
            }
        }
    }

    pub fn load() -> Accounts {
        if in_file() {
            return Accounts { kept: file_accounts(), unreadable: vec![] };
        }
        let mut st = STATE.lock().unwrap();
        refresh(&mut st);
        let mut a = st.read.clone().unwrap_or_default();
        // Not moved into the Keychain yet (it was locked, say): still yours, from the file.
        for (n, t) in file_accounts() {
            if !a.taken(n) && !a.kept.iter().any(|(_, k)| *k == t) {
                a.kept.push((n, t));
            }
        }
        a.kept.sort();
        a
    }

    pub fn problem() -> Option<String> {
        if in_file() {
            return None;
        }
        let mut st = STATE.lock().unwrap();
        refresh(&mut st);
        st.problem.clone()
    }

    fn write(kc: &SecKeychain, changes: &[(u32, Option<&str>)]) -> Result<(), String> {
        let _quiet = SecKeychain::disable_user_interaction().map_err(|e| said(&e))?;
        let service = service();
        for (n, t) in changes {
            let account = format!("Account {n}");
            match t {
                Some(t) => kc.set_generic_password(&service, &account, t.trim().as_bytes()).map_err(|e| said(&e))?,
                None => match kc.find_generic_password(&service, &account) {
                    Ok((_, item)) => item.delete(),
                    Err(e) if e.code() == -25300 => {}
                    Err(e) => return Err(said(&e)),
                },
            }
        }
        Ok(())
    }

    pub fn set(changes: &[(u32, Option<&str>)]) -> anyhow::Result<()> {
        if in_file() {
            return file_set(changes);
        }
        let mut st = STATE.lock().unwrap();
        let kc = keychain().map_err(|e| anyhow::anyhow!("dino couldn't open the Keychain: {}", said(&e)))?;
        let wrote = write(&kc, changes);
        // As the Keychain has it now, whatever happened.
        st.read = None;
        st.failed_at = None;
        refresh(&mut st);
        wrote.map_err(|e| anyhow::anyhow!("dino couldn't keep the account in your Keychain: {e}"))?;
        // A number the file still held (not moved yet) is the Keychain's now.
        let stale: Vec<(u32, Option<&str>)> = changes.iter().filter(|(n, _)| file_accounts().iter().any(|(m, _)| m == n)).map(|(n, _)| (*n, None)).collect();
        if !stale.is_empty() {
            file_set(&stale)?;
        }
        Ok(())
    }

    pub fn migrate() -> anyhow::Result<Vec<u32>> {
        if in_file() {
            return Ok(vec![]);
        }
        let file = file_accounts();
        if file.is_empty() {
            return Ok(vec![]);
        }
        let mut st = STATE.lock().unwrap();
        let kc = keychain().map_err(|e| anyhow::anyhow!("{}", said(&e)))?;
        let before = read(&kc).map_err(|e| anyhow::anyhow!("{e}"))?;
        let moves = placement(&file, &before);
        let changes: Vec<(u32, Option<&str>)> = moves.iter().map(|(n, t)| (*n, Some(t.as_str()))).collect();
        write(&kc, &changes).map_err(|e| anyhow::anyhow!("{e}"))?;
        // Read back: only what the Keychain now holds leaves the file.
        let after = read(&kc).map_err(|e| anyhow::anyhow!("{e}"))?;
        st.read = Some(after.clone());
        st.problem = None;
        st.failed_at = None;
        drop(st);
        let safe: Vec<u32> = file.iter().filter(|(_, t)| after.kept.iter().any(|(_, k)| k == t)).map(|(n, _)| *n).collect();
        file_set(&safe.iter().map(|n| (*n, None)).collect::<Vec<_>>())?;
        Ok(moves.iter().map(|(n, _)| *n).filter(|n| after.kept.iter().any(|(m, _)| m == n)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "sk-ant-oat01-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const B: &str = "sk-ant-oat01-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
    const C: &str = "sk-ant-oat01-CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";

    /// Moving the file's tokens in: one there already isn't added again (a move stopped halfway,
    /// run again), and a number another token holds gives way to the first free one, past the
    /// numbers the file itself still uses and those of unreadable items.
    #[test]
    fn migration_places_each_token_once() {
        let kc = Accounts { kept: vec![(2, A.into())], unreadable: vec![3] };
        assert_eq!(placement(&[(2, A.into())], &kc), vec![], "there already");
        assert_eq!(placement(&[(2, B.into()), (4, C.into())], &kc), vec![(5, B.into()), (4, C.into())]);
        assert_eq!(placement(&[(6, B.into())], &Accounts::default()), vec![(6, B.into())]);
        assert!(kc.taken(3) && kc.taken(2) && !kc.taken(4));
    }

    /// The real Keychain, in a keychain file of the test's own (never the login one), by hand:
    /// `cargo test -p dino-core --lib -- --ignored account_store::tests::keychain`. Tokens move
    /// in from the key file once (again, as after a stop halfway: nothing doubled), are added,
    /// read, removed; an item dino can't read (another app's, trusting only itself) reads as
    /// unreadable, never asking; and dino's items trust dino's own binary alone.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn keychain() {
        use std::process::Command;
        let dir = std::env::temp_dir().join(format!("dino-keychain-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let kc = dir.join("test.keychain-db");
        let svc = format!("dino Claude account (test {})", std::process::id());
        let pw = "dino-test-keychain";
        let sec = |args: &[&str]| Command::new("/usr/bin/security").args(args).output().unwrap();
        let lists = || String::from_utf8_lossy(&sec(&["list-keychains", "-d", "user"]).stdout).to_string();
        let before = lists();
        assert!(sec(&["create-keychain", "-p", pw, kc.to_str().unwrap()]).status.success());
        // `delete-keychain` at the end takes it off the user's search list again, and only it.
        assert!(sec(&["unlock-keychain", "-p", pw, kc.to_str().unwrap()]).status.success());
        let done = || {
            let _ = sec(&["delete-keychain", kc.to_str().unwrap()]);
            let _ = std::fs::remove_dir_all(&dir);
        };
        // SAFETY: only this test reads these, and dino-core's other tests never reach the store.
        unsafe {
            std::env::set_var("DINO_KEYCHAIN", &kc);
            std::env::set_var("DINO_KEYCHAIN_SERVICE", &svc);
            std::env::set_var("DINO_HOME", &dir);
            std::env::remove_var("DINO_CLAUDE_ACCOUNT_STORE");
        }
        let run = std::panic::catch_unwind(|| {
            crate::settings::set_keys(&[("CLAUDE_ACCOUNT_2", Some(A)), ("CLAUDE_ACCOUNT_3", Some(B)), ("OTHER_KEY", Some("x"))]).unwrap();
            assert_eq!(migrate().unwrap(), vec![2, 3]);
            let file = std::fs::read_to_string(crate::keys_file()).unwrap();
            assert!(!file.contains("CLAUDE_ACCOUNT") && file.contains("OTHER_KEY=x"), "moved out of the file, the rest left");
            assert_eq!(load().kept, vec![(2, A.to_string()), (3, B.to_string())]);
            // Stopped before the file let go: run again, nothing doubled.
            crate::settings::set_keys(&[("CLAUDE_ACCOUNT_2", Some(A))]).unwrap();
            assert_eq!(migrate().unwrap(), Vec::<u32>::new());
            assert!(!std::fs::read_to_string(crate::keys_file()).unwrap().contains("CLAUDE_ACCOUNT"));
            set(&[(4, Some(C))]).unwrap();
            set(&[(2, None)]).unwrap();
            assert_eq!(load().kept, vec![(3, B.to_string()), (4, C.to_string())]);
            let keys = with_accounts([("CLAUDE_ACCOUNT_9".to_string(), "stale".to_string())].into());
            assert_eq!(keys.get("CLAUDE_ACCOUNT_4").map(String::as_str), Some(C));
            assert!(!keys.contains_key("CLAUDE_ACCOUNT_9") && !keys.contains_key("CLAUDE_ACCOUNT_2"));
            // dino's item trusts dino's binary (this test's), nothing else.
            let acl = String::from_utf8_lossy(&sec(&["dump-keychain", "-a", kc.to_str().unwrap()]).stdout).to_string();
            let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
            let item = acl.split("keychain: ").find(|i| i.contains("\"Account 3\"")).unwrap().to_string();
            assert!(item.contains(&*exe.to_string_lossy()), "{item}");
            assert!(!item.contains("/usr/bin/security"), "{item}");
            // Another app's item under the service, trusting only itself: unreadable, its number
            // taken, and no prompt.
            assert!(
                sec(&["add-generic-password", "-s", &svc, "-a", "Account 5", "-w", "sk-ant-oat01-someone-elses-0000000000000000000000000000", "-T", "/usr/bin/security", kc.to_str().unwrap()])
                    .status
                    .success()
            );
            set(&[]).unwrap();
            let a = load();
            assert_eq!(a.unreadable, vec![5]);
            assert!(a.taken(5));
            assert!(problem().is_none());
            // Removed: gone from the Keychain.
            set(&[(3, None), (4, None)]).unwrap();
            assert!(load().kept.is_empty());
            let left = String::from_utf8_lossy(&sec(&["dump-keychain", kc.to_str().unwrap()]).stdout).to_string();
            assert!(!left.contains("\"Account 3\"") && !left.contains("\"Account 4\""));
        });
        done();
        assert_eq!(lists(), before, "the user's search list as it was");
        if let Err(e) = run {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    fn keychain_item_names() {
        assert_eq!(number("Account 3"), Some(3));
        assert_eq!(number("Account 1"), None, "1 is Claude Code's own");
        assert_eq!(number("account 3"), None);
        assert_eq!(number("Account x"), None);
    }
}
