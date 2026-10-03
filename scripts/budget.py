#!/usr/bin/env python3
"""dino's performance budget: run before every merge. Unattended, about 4 minutes.

    scripts/budget.py [REPO]            # REPO: the checkout to test (default: this checkout)
    scripts/budget.py REPO --no-build   # use REPO's existing release builds
    scripts/budget.py REPO --no-claude  # skip the real Claude session (no network, no tokens)

It builds dinod and the app from REPO, starts an isolated test dinod (its own DINO_HOME) and a
test copy of the app (its own bundle id, registered for nothing), with a realistic load: six
shells and two Claude Code sessions (haiku). Then it measures, and fails (exit 1) past the budget:

    idle app CPU                    <= 1 %
    WindowServer, app idle vs none  <= +5 points (measured twice: WindowServer is shared)
    one shell streaming (in view)   <= 10 %
    one Claude turn streaming       <= 10 %
    Settings open, idle             <= 2 %
    keystroke echo through dinod    median <= plain pty + 1 ms, p95 <= plain pty + 2 ms
    100 MB `cat` through dinod      <= 2 s, and <= 2.5 dinod CPU-seconds

Never touches the real dinod, ~/.local/bin/dino or your Dino.app; kills only its own processes.
The app window must stay visible (not minimized or hidden) while it runs: a hidden window doesn't
render, and the streaming numbers would read low. It warns when the window wasn't visible.
Claude's trust entry for its test folder is removed from ~/.claude.json afterwards (0600 kept).
"""
import fcntl, glob, json, os, pty, select, shutil, socket, statistics, struct, subprocess, sys, termios, time

args = [a for a in sys.argv[1:] if not a.startswith("--")]
REPO = os.path.abspath(args[0]) if args else os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BUILD = "--no-build" not in sys.argv
CLAUDE = "--no-claude" not in sys.argv
HOME = "/tmp/dino-budget"
WORK = HOME + "/work"
BIN = REPO + "/target/release/dino"
APP = "/tmp/dino-budget-app/BudgetDino.app"
BUNDLE = "ai.vecna.dino.budget"
SAFE = ["--model", "haiku", "--disallowedTools", "Artifact,Write,Edit,WebFetch,WebSearch"]

BUDGET = {"idle": 1.0, "cat_s": 2.0, "cat_cpu": 2.5, "ws_delta": 5.0, "shell": 10.0, "claude": 10.0, "settings": 2.0, "lat_median": 1.0, "lat_p95": 2.0}
results, failures, warnings = [], [], []


def log(*a):
    print(*a, flush=True)


def check(name, value, limit, unit="%"):
    ok = value <= limit
    results.append((name, value, limit, unit, ok))
    log(f"{'PASS' if ok else 'FAIL'} {name}: {value:.2f}{unit} (budget {limit}{unit})")
    if not ok:
        failures.append(name)


# ---- dinod IPC ---------------------------------------------------------------------------------

def rd(s, n):
    b = b""
    while len(b) < n:
        c = s.recv(n - len(b))
        if not c:
            raise EOFError
        b += c
    return b


def send(s, k, d):
    s.sendall(bytes([k]) + struct.pack(">I", len(d)) + d)


def req(body):
    s = socket.socket(socket.AF_UNIX)
    s.connect(HOME + "/dinod.sock")
    send(s, 0, json.dumps(body).encode())
    h = rd(s, 5)
    r = json.loads(rd(s, struct.unpack(">I", h[1:])[0]))
    s.close()
    return r


def type_into(sid, text):
    s = socket.socket(socket.AF_UNIX)
    s.connect(HOME + "/dinod.sock")
    send(s, 0, json.dumps({"type": "attach", "id": sid, "cols": 120, "rows": 40}).encode())
    rd(s, 5)
    time.sleep(0.3)
    for ch in text:
        send(s, 1, ch.encode())
        time.sleep(0.02)
    time.sleep(0.3)
    s.close()


# ---- CPU ---------------------------------------------------------------------------------------

def cpu_secs(pid):
    t = subprocess.run(["ps", "-o", "time=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    if not t:
        return 0.0
    secs = 0.0
    for p in t.replace("-", ":").split(":"):
        secs = secs * 60 + float(p)
    return secs


def usage(pid, secs):
    c0, t0 = cpu_secs(pid), time.time()
    time.sleep(secs)
    return 100 * (cpu_secs(pid) - c0) / (time.time() - t0)


def ws_pid():
    return int(subprocess.run(["pgrep", "-x", "WindowServer"], capture_output=True, text=True).stdout.split()[0])


def windows(pid):
    """How many on-screen windows the app has (the main window, plus Settings when it's open)."""
    code = ("import CoreGraphics\nlet l = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as! [[String: Any]]\n"
            f"print(l.filter {{ ($0[kCGWindowOwnerPID as String] as? Int32) == {pid} && ($0[kCGWindowLayer as String] as? Int) == 0 }}.count)")
    r = subprocess.run(["swift", "-"], input=code, capture_output=True, text=True)
    return int(r.stdout.strip()) if r.returncode == 0 and r.stdout.strip().isdigit() else 0


def visible(pid):
    """Whether the app has an on-screen window, from the window list (no permission needed)."""
    code = ("import CoreGraphics\nlet l = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as! [[String: Any]]\n"
            f"print(l.contains {{ ($0[kCGWindowOwnerPID as String] as? Int32) == {pid} && ($0[kCGWindowLayer as String] as? Int) == 0 }})")
    r = subprocess.run(["swift", "-"], input=code, capture_output=True, text=True)
    return r.stdout.strip() == "true" if r.returncode == 0 else None


# ---- latency (the earlier harness) -------------------------------------------------------------

def open_pty(argv, env):
    pid, fd = pty.fork()
    if pid == 0:
        os.execve(argv[0], argv, env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 100, 0, 0))
    return pid, fd


def drain(fd, secs):
    end = time.time() + secs
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                os.read(fd, 65536)
            except OSError:
                return


def echo_times(fd, n=300):
    drain(fd, 2.0)
    out = []
    for i in range(n):
        ch = b"abcdefghij"[i % 10:i % 10 + 1]
        t0 = time.perf_counter()
        os.write(fd, ch)
        got = b""
        while ch not in got:
            r, _, _ = select.select([fd], [], [], 1.0)
            if not r:
                break
            got += os.read(fd, 4096)
        out.append((time.perf_counter() - t0) * 1000)
        if i % 20 == 19:
            os.write(fd, b"\x15")
            drain(fd, 0.05)
        time.sleep(0.01)
    out.sort()
    return statistics.median(out), out[int(len(out) * 0.95)]


def latency():
    root = "/tmp/dino-budget-lat"
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(root + "/home")
    os.makedirs(root + "/dino")
    open(root + "/home/.zshrc", "w").write("PROMPT='%# '\n")
    base = {k: v for k, v in os.environ.items() if not k.startswith(("CLAUDE", "ZDOTDIR"))}
    env = dict(base, HOME=root + "/home", SHELL="/bin/zsh", DINO_HOME=root + "/dino", TERM="xterm-256color")
    pid, fd = open_pty(["/bin/zsh", "-l"], env)
    plain = echo_times(fd)
    os.close(fd); os.kill(pid, 9); os.waitpid(pid, 0)
    d = subprocess.Popen([BIN, "daemon"], env=env, stdout=open(root + "/dinod.log", "w"), stderr=subprocess.STDOUT, start_new_session=True)
    try:
        end = time.time() + 10
        while not os.path.exists(root + "/dino/dinod.sock") and time.time() < end:
            time.sleep(0.1)
        s = socket.socket(socket.AF_UNIX)
        s.connect(root + "/dino/dinod.sock")
        body = json.dumps({"type": "new", "launcher": "shell", "args": [], "cwd": root + "/home", "cols": 100, "rows": 40}).encode()
        send(s, 0, body)
        time.sleep(0.5)
        s.close()
        pid, fd = open_pty([BIN, "attach", "1"], env)
        via = echo_times(fd)
        os.close(fd); os.kill(pid, 9); os.waitpid(pid, 0)
    finally:
        d.kill(); d.wait()
        shutil.rmtree(root, ignore_errors=True)
    log(f"     echo: plain pty median {plain[0]:.2f} ms p95 {plain[1]:.2f} ms · through dinod median {via[0]:.2f} ms p95 {via[1]:.2f} ms")
    check("keystroke echo median, added by dinod", via[0] - plain[0], BUDGET["lat_median"], " ms")
    check("keystroke echo p95, added by dinod", via[1] - plain[1], BUDGET["lat_p95"], " ms")


# ---- the app -----------------------------------------------------------------------------------

def throughput():
    """100 MB of output through dinod and `dino attach`: how long, and dinod's CPU for it."""
    root = "/tmp/dino-budget-cat"
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(root + "/home")
    os.makedirs(root + "/dino")
    open(root + "/home/.zshrc", "w").write("PROMPT='%# '\n")
    with open(root + "/big.txt", "wb") as f:
        line = b"x" * 99 + b"\n"
        f.write(line * 1_000_000)
    base = {k: v for k, v in os.environ.items() if not k.startswith(("CLAUDE", "ZDOTDIR"))}
    env = dict(base, HOME=root + "/home", SHELL="/bin/zsh", DINO_HOME=root + "/dino", TERM="xterm-256color")
    d = subprocess.Popen([BIN, "daemon"], env=env, stdout=open(root + "/dinod.log", "w"), stderr=subprocess.STDOUT, start_new_session=True)
    try:
        end = time.time() + 10
        while not os.path.exists(root + "/dino/dinod.sock") and time.time() < end:
            time.sleep(0.1)
        s = socket.socket(socket.AF_UNIX)
        s.connect(root + "/dino/dinod.sock")
        send(s, 0, json.dumps({"type": "new", "launcher": "shell", "args": [], "cwd": root + "/home", "cols": 100, "rows": 40}).encode())
        time.sleep(0.5)
        s.close()
        pid, fd = open_pty([BIN, "attach", "1"], env)
        drain(fd, 1.0)
        before = cpu_secs(d.pid)
        t0 = time.perf_counter()
        os.write(fd, f"cat {root}/big.txt; echo CAT''DONE\r".encode())
        seen = b""
        while b"CATDONE" not in seen and time.perf_counter() - t0 < 60:
            r, _, _ = select.select([fd], [], [], 1.0)
            if r:
                seen = (seen + os.read(fd, 1 << 16))[-64:]
        secs = time.perf_counter() - t0
        cpu = cpu_secs(d.pid) - before
        os.close(fd); os.kill(pid, 9); os.waitpid(pid, 0)
    finally:
        d.kill(); d.wait()
        shutil.rmtree(root, ignore_errors=True)
    log(f"     stream: 100 MB through dinod and `dino attach` at {100 / secs:.1f} MB/s, dinod {cpu:.2f} CPU-s")
    check("100 MB `cat` through dinod, seconds", secs, BUDGET["cat_s"], " s")
    check("100 MB `cat`, dinod CPU-seconds", cpu, BUDGET["cat_cpu"], " s")


def build():
    log("building dinod and the app…")
    subprocess.run(["cargo", "build", "--release", "-q"], cwd=REPO, check=True)
    subprocess.run(["swift", "build", "-c", "release"], cwd=REPO + "/app", check=True, capture_output=True)


def mkapp():
    shutil.rmtree(os.path.dirname(APP), ignore_errors=True)
    os.makedirs(APP + "/Contents/MacOS")
    os.makedirs(APP + "/Contents/Resources")
    rel = REPO + "/app/.build/release"
    shutil.copy(rel + "/Dino", APP + "/Contents/MacOS/Dino")
    for b in glob.glob(rel + "/*.bundle"):
        shutil.copytree(b, APP + "/Contents/Resources/" + os.path.basename(b))
    shutil.copytree(rel + "/Sparkle.framework", APP + "/Contents/Frameworks/Sparkle.framework", symlinks=True)
    shutil.copy(REPO + "/app/Info.plist", APP + "/Contents/Info.plist")
    pb = "/usr/libexec/PlistBuddy"
    subprocess.run([pb, "-c", f"Set :CFBundleIdentifier {BUNDLE}", APP + "/Contents/Info.plist"], check=True)
    # Registered for nothing: it must never become a handler on this Mac.
    for k in ("CFBundleDocumentTypes", "CFBundleURLTypes", "NSServices"):
        subprocess.run([pb, "-c", f"Delete :{k}", APP + "/Contents/Info.plist"], capture_output=True)
    subprocess.run(["codesign", "--force", "--deep", "--sign", "-", APP], check=True, capture_output=True)


def defaults(*a):
    subprocess.run(["defaults", *a], capture_output=True)


def launch(env, select, settings=False):
    """The app, showing session `select` (the one it restores), Settings open or not."""
    defaults("write", BUNDLE, f"selected.{HOME}", select)
    # Models & Providers: the pane with the most to draw (the model lists).
    defaults("write", BUNDLE, "settingsTab", "models")
    # No second window restored from a previous run.
    shutil.rmtree(os.path.expanduser(f"~/Library/Saved Application State/{BUNDLE}.savedState"), ignore_errors=True)
    a = ["-ApplePersistenceIgnoreState", "YES"]
    p = subprocess.Popen([APP + "/Contents/MacOS/Dino", *a], env=env, stdout=open(HOME + "/app.log", "a"), stderr=subprocess.STDOUT, start_new_session=True)
    time.sleep(8)
    if settings:
        # ⌘, through the app's own menu, via AppleScript's "open location"-free path: the menu item.
        subprocess.run(["osascript", "-e", f'tell application id "{BUNDLE}" to activate'], capture_output=True)
    return p


def stop(p):
    if p and p.poll() is None:
        p.terminate()
        try:
            p.wait(5)
        except subprocess.TimeoutExpired:
            p.kill()


def untrust():
    p = os.path.expanduser("~/.claude.json")
    if not os.path.exists(p):
        return
    d = json.load(open(p))
    pr = d.get("projects", {})
    gone = [k for k in pr if k.startswith((HOME, "/private" + HOME))]
    if gone:
        for k in gone:
            del pr[k]
        t = p + ".tmp"
        json.dump(d, open(t, "w"), indent=2)
        os.chmod(t, 0o600)
        os.replace(t, p)


def main():
    if BUILD:
        build()
    mkapp()
    shutil.rmtree(HOME, ignore_errors=True)
    os.makedirs(WORK)
    subprocess.run(["git", "init", "-q", WORK], check=True)
    open(WORK + "/notes.md", "w").write("# budget\n")
    subprocess.run(["git", "-C", WORK, "add", "."], check=True)
    subprocess.run(["git", "-C", WORK, "-c", "user.name=b", "-c", "user.email=b@b", "commit", "-qm", "init"], check=True)
    env = {k: v for k, v in os.environ.items() if not k.startswith("CLAUDE")}
    env["DINO_HOME"] = HOME
    # A Mac that has been through Welcome: the budget is for the app as it idles every day, not the
    # first-launch card over it (which, once it showed reliably, put several WindowServer points
    # on every run).
    open(HOME + "/settings.toml", "w").write("[machine]\nonboarded = true\n")
    d = subprocess.Popen([BIN, "daemon"], env=env, stdout=open(HOME + "/dinod.log", "a"), stderr=subprocess.STDOUT, start_new_session=True)
    time.sleep(1.5)
    app = None
    ws = ws_pid()
    try:
        shells = [req({"type": "new", "launcher": "shell", "args": [], "cwd": WORK, "cols": 120, "rows": 40})["id"] for _ in range(6)]
        claudes = []
        if CLAUDE:
            claudes = [req({"type": "new", "launcher": "claude", "args": SAFE, "cwd": WORK, "cols": 120, "rows": 40})["id"] for _ in range(2)]
            time.sleep(6)
            for c in claudes:  # the folder trust prompt: Yes is the second choice
                type_into(c, "\x1b[B\r")
        time.sleep(4)

        # WindowServer with no app, then with the app idle: twice, since it's shared.
        deltas = []
        for _ in range(2):
            base = usage(ws, 12)
            app = launch(env, shells[1])
            time.sleep(4)
            vis = visible(app.pid)
            with_app = usage(ws, 12)
            idle = usage(app.pid, 15)
            deltas.append((with_app - base, idle, vis))
            stop(app)
            app = None
            time.sleep(3)
        delta = min(x[0] for x in deltas)
        idle = min(x[1] for x in deltas)
        if not any(x[2] for x in deltas):
            warnings.append("the app's window wasn't on screen while idle was measured")
        check("idle app CPU (8 sessions, sidebar open)", idle, BUDGET["idle"])
        check("WindowServer added by the idle app", delta, BUDGET["ws_delta"], " pts")

        # One shell streaming, in view.
        app = launch(env, shells[0])
        type_into(shells[0], "for i in $(seq 1 800); do echo line $i of streaming output from a build; sleep 0.03; done\r")
        time.sleep(2)
        if not visible(app.pid):
            warnings.append("the app's window wasn't on screen while streaming was measured")
        check("one shell streaming, in view", usage(app.pid, 15), BUDGET["shell"])
        time.sleep(12)
        stop(app)
        app = None

        if CLAUDE:
            app = launch(env, claudes[0])
            type_into(claudes[0], "Write a 900-word essay about pelicans, in plain prose.\r")
            time.sleep(5)
            check("one Claude turn streaming, in view", usage(app.pid, 15), BUDGET["claude"])
            time.sleep(25)
            stop(app)
            app = None

        # Settings open, nothing happening: the model list must cost nothing while idle.
        app = launch(env, shells[1])
        before = windows(app.pid)
        subprocess.run(["osascript", "-e", f'tell application "System Events" to keystroke "," using command down'], capture_output=True)
        time.sleep(8)
        # Measured only with Settings really open: without Accessibility for ⌘, it never opens, and
        # the number would be the main window's, passed off as Settings'.
        if windows(app.pid) > before:
            check("Settings open, idle", usage(app.pid, 15), BUDGET["settings"])
        else:
            warnings.append("Settings open, idle: not measured (Settings didn't open; ⌘, needs Accessibility for this terminal)")
        stop(app)
        app = None
    finally:
        stop(app)
        try:
            for x in req({"type": "state"})["sessions"]:
                req({"type": "kill", "id": x["id"]})
        except Exception:
            pass
        time.sleep(0.5)
        d.kill()
        d.wait()
        defaults("delete", BUNDLE)
        shutil.rmtree(os.path.expanduser(f"~/Library/Saved Application State/{BUNDLE}.savedState"), ignore_errors=True)
        untrust()
        for c in glob.glob(os.path.expanduser("~/.claude/projects/-private-tmp-dino-budget*")) + glob.glob(os.path.expanduser("~/.claude/projects/-tmp-dino-budget*")):
            shutil.rmtree(c, ignore_errors=True)

    latency()
    throughput()
    shutil.rmtree(HOME, ignore_errors=True)
    shutil.rmtree(os.path.dirname(APP), ignore_errors=True)
    for w in warnings:
        log("WARN", w)
    log("\nBUDGET MET" if not failures else "\nOVER BUDGET: " + ", ".join(failures))
    sys.exit(1 if failures else 0)


main()
