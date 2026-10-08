#!/usr/bin/env python3
"""Write THIRD_PARTY_NOTICES.md: the third-party code Dino.app and the dino CLI ship, with the full
text of every license, each text once. release.sh puts the file in the app (Contents/Resources,
which the About window shows) and in the CLI tarball beside LICENSE.

    scripts/third-party.py            # after changing dependencies; commit the result
    scripts/third-party.py --check    # fail if THIRD_PARTY_NOTICES.md isn't what this writes (CI)
    scripts/third-party.py --binary Dino.app
                                      # fail if the app links a library this file doesn't name

The Rust crates in the dino binary come from cargo-about (CARGO_ABOUT below), with
packaging/licenses/about.toml: the crates Cargo.lock resolves for dino on macOS, without dev and
build dependencies, and the license files each crate carries.

Everything else is listed by hand in NATIVE, with its license texts in packaging/licenses/native/.
When app/Package.swift moves a package, the check fails until NATIVE says the new version. For
libghostty-spm, check what its libghostty.a puts into the app first: release.sh builds the app and
runs --binary, which finds the libraries Ghostty can link by their symbols; Ghostty's
src/build/SharedDeps.zig and build.zig.zon (at the commit libghostty-spm's Ghostty.ref names) say
which Zig packages and C libraries a macOS build of the library compiles in.
"""
import argparse, hashlib, json, os, re, shutil, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "THIRD_PARTY_NOTICES.md")
LICENSES = os.path.join(ROOT, "packaging", "licenses")
# The output depends on cargo-about's version (how it finds and cleans license texts), so CI and
# everyone else run the same one: cargo install --locked --features cli cargo-about@<this>.
CARGO_ABOUT = "0.9.2"

# The app's Swift packages, as app/Package.swift pins them. NATIVE below was checked at these.
PACKAGES = {"libghostty-spm": "1.6.20260929", "MSDisplayLink": "2.2.1", "Sparkle": "2.10.0"}

GHOSTTY = "0538f7535be0cbca6bbe54e6fde654d5c628f1f2"  # libghostty-spm 1.6.20260929's Ghostty.ref

# What ships besides the Rust crates: (name, version, source, what it is and where it ships,
# SPDX license, [(license, file in packaging/licenses/native/ or a path in this repository)],
# symbols: a regex nm finds in Contents/MacOS/Dino when the app links it, or None).
NATIVE = [
    ("Ghostty", f"1.3.2-dev ({GHOSTTY[:12]})", "https://github.com/ghostty-org/ghostty",
     "The terminal engine, libghostty, linked into the app from libghostty-spm. Also its terminfo "
     "entries (in the app and the dino binary) and its fish, elvish and nushell shell integration "
     "(in the dino binary).",
     "MIT", [("MIT License", "native/ghostty.txt")], r"^_ghostty_"),
    ("libghostty-spm", PACKAGES["libghostty-spm"], "https://github.com/Lakr233/libghostty-spm",
     "Builds libghostty for the app. Its Swift wrapper GhosttyTerminal is in the app as DinoGhostty, "
     "and its bash and zsh shell integration in the dino binary.",
     "MIT", [("MIT License", "app/Sources/DinoGhostty/LICENSE")], None),
    ("Zig's standard library and compiler_rt", "", "https://codeberg.org/ziglang/zig",
     "Compiled into libghostty by Zig.", "MIT", [("MIT License", "native/zig.txt")], None),
    ("libxev", "9ce8e8e", "https://github.com/mitchellh/libxev", "Compiled into libghostty.",
     "MIT", [("MIT License", "native/libxev.txt")], None),
    ("libvaxis", "1dbbe57", "https://github.com/rockorager/libvaxis", "Compiled into libghostty.",
     "MIT", [("MIT License", "native/libvaxis.txt")], None),
    ("z2d", "7dbae85", "https://github.com/vancluever/z2d",
     "Compiled into libghostty, unmodified. Its source, under the MPL-2.0, is at the address beside it.",
     "MPL-2.0", [("z2d's license and notices", "native/z2d.txt"),
                 ("Mozilla Public License 2.0", "native/MPL-2.0.txt")], None),
    ("zig-objc", "c8de82f", "https://github.com/mitchellh/zig-objc", "Compiled into libghostty.",
     "MIT", [("MIT License", "native/zig-objc.txt")], None),
    ("zf", "c35c421", "https://github.com/natecraddock/zf", "Compiled into libghostty.",
     "MIT", [("MIT License", "native/zf.txt")], None),
    ("uucode", "9d55524", "https://github.com/jacobsandlund/uucode",
     "Unicode tables and code compiled into libghostty.", "MIT AND Unicode-3.0",
     [("MIT License", "native/uucode.txt"), ("MIT License", "native/uucode-hoehrmann.txt"),
      ("Unicode License v3", "native/uucode-unicode.txt")], None),
    ("Oniguruma", "6.9.9", "https://github.com/kkos/oniguruma", "Linked into the app by libghostty.",
     "BSD-2-Clause", [("BSD 2-Clause License", "native/oniguruma.txt")], r"^_onig_"),
    ("simdutf", "5.2.8", "https://github.com/simdutf/simdutf", "Linked into the app by libghostty.",
     "Apache-2.0 OR MIT (used under MIT)", [("MIT License", "native/simdutf.txt")], r"simdutf"),
    ("Highway", "1.2.0 (66486a1)", "https://github.com/google/highway",
     "Linked into the app by libghostty.", "Apache-2.0 OR BSD-3-Clause (used under BSD-3-Clause)",
     [("BSD 3-Clause License", "native/highway.txt")], r"3hwy"),
    ("Wuffs", "0.4 (7411f48)", "https://github.com/google/wuffs", "Linked into the app by libghostty.",
     "Apache-2.0 OR MIT (used under MIT)", [("MIT License", "native/wuffs.txt")], r"^_wuffs_"),
    ("libintl, from GNU gettext", "0.24", "https://www.gnu.org/software/gettext/",
     "Linked into the app by libghostty, unmodified. Its source is in "
     "https://ftp.gnu.org/gnu/gettext/gettext-0.24.tar.gz (gettext-runtime/intl), and the source of "
     "everything else linked with it is public too (dino, libghostty-spm and Ghostty above), so the "
     "app can be rebuilt with a modified libintl.",
     "LGPL-2.1-or-later", [("GNU Lesser General Public License 2.1", "native/LGPL-2.1.txt")],
     r"libintl_|^_bindtextdomain$"),
    ("JetBrains Mono", "2.304", "https://github.com/JetBrains/JetBrainsMono",
     "Font, embedded in libghostty.", "OFL-1.1",
     [("SIL Open Font License 1.1", "native/jetbrains-mono.txt")], None),
    ("Symbols Nerd Font", "3.4.0", "https://github.com/ryanoasis/nerd-fonts",
     "Font, embedded in libghostty. Its icons come from several icon sets, each under its own "
     "license: see https://github.com/ryanoasis/nerd-fonts/blob/v3.4.0/license-audit.md.",
     "MIT AND OFL-1.1", [("Nerd Fonts licensing", "native/nerd-fonts.txt")], None),
    ("iTerm2-Color-Schemes", "", "https://github.com/mbadolato/iTerm2-Color-Schemes",
     "Ghostty 1.3.1's color themes, generated from it, in the app's resources.",
     "MIT", [("MIT License", "app/Sources/DinoGhostty/Resources/Ghostty/LICENSE-themes")], None),
    ("bash-preexec", "", "https://github.com/rcaloras/bash-preexec",
     "In the dino binary's bash integration.", "MIT",
     [("MIT License", "crates/dino-daemon/shell-integration/bash/LICENSE-bash-preexec.md")], None),
    ("MSDisplayLink", PACKAGES["MSDisplayLink"], "https://github.com/Lakr233/MSDisplayLink",
     "Linked into the app.", "MIT", [("MIT License", "native/msdisplaylink.txt")], None),
    ("Sparkle", PACKAGES["Sparkle"], "https://github.com/sparkle-project/Sparkle",
     "The app's updater: Contents/Frameworks/Sparkle.framework. Includes bsdiff, sais-lite, "
     "orlp/ed25519 and SUSignatureVerifier, whose notices are in its license.",
     "MIT AND BSD-2-Clause AND Zlib", [("Sparkle's license", "native/sparkle.txt")], None),
]

# Libraries libghostty.a carries, or Ghostty can build in, that the app doesn't link today: if one
# of these shows up in the app, it needs an entry in NATIVE first (--binary fails until it has one).
UNLISTED = {
    "FreeType": r"^_FT_", "libpng": r"^_png_", "zlib": r"^_(deflate|inflate|adler32|crc32)(Init.*|End|_.*)?$",
    "HarfBuzz": r"^_hb_", "fontconfig": r"^_Fc[A-Z]", "Dear ImGui": r"ImGui", "glslang": r"glslang",
    "SPIRV-Cross": r"spirv_cross", "sentry-native": r"^_sentry_", "Breakpad": r"google_breakpad",
    "stb": r"^_stbi?_",
}


def fail(msg):
    print(f"third-party.py: {msg}", file=sys.stderr)
    sys.exit(1)


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def check_packages():
    text = read(os.path.join(ROOT, "app", "Package.swift"))
    pinned = {url.rstrip("/").removesuffix(".git").split("/")[-1]: v
              for url, v in re.findall(r'\.package\(url: "([^"]+)", exact: "([^"]+)"\)', text)}
    if pinned != PACKAGES:
        fail(f"app/Package.swift pins {pinned}, but NATIVE was checked at {PACKAGES}: check what the "
             "new versions ship and link (this script's docstring says how), then update NATIVE, "
             "its license texts and PACKAGES")


def cargo_about():
    exe = shutil.which("cargo-about")
    if not exe:
        fail(f"needs cargo-about {CARGO_ABOUT}: cargo install --locked --features cli cargo-about@{CARGO_ABOUT}")
    version = subprocess.run([exe, "--version"], capture_output=True, text=True).stdout.split()[-1]
    if version != CARGO_ABOUT:
        fail(f"cargo-about is {version}, this file is written by {CARGO_ABOUT}: "
             f"cargo install --locked --features cli cargo-about@{CARGO_ABOUT}")
    # Offline, after fetching what Cargo.lock names: the license files the crates carry, and
    # nothing looked up on the network, so every machine writes the same file.
    subprocess.run(["cargo", "fetch", "--locked", "-q"], cwd=ROOT, check=True)
    out = subprocess.run(
        [exe, "generate", "--frozen", "--fail", "--format", "json",
         "-c", os.path.join(LICENSES, "about.toml"),
         "-m", os.path.join(ROOT, "crates", "dino", "Cargo.toml")],
        cwd=ROOT, capture_output=True, text=True)
    if out.returncode != 0:
        fail(f"cargo about generate failed:\n{out.stderr}")
    return json.loads(out.stdout)


def clean(text):
    lines = [l.rstrip() for l in text.replace("\r\n", "\n").split("\n")]
    while lines and not lines[0]:
        lines.pop(0)
    while lines and not lines[-1]:
        lines.pop()
    return "\n".join(lines)


def fenced(text):
    longest = max((len(m) for m in re.findall(r"`+", text)), default=0)
    fence = "`" * max(3, longest + 1)
    return f"{fence}text\n{text}\n{fence}"


def render(about):
    # Each license text once, with everything that ships under it: [name, text, [users]].
    texts = {}

    def use(name, text, user):
        text = clean(text)
        key = hashlib.sha256(text.encode()).hexdigest()
        entry = texts.setdefault(key, [name, text, []])
        if user not in entry[2]:
            entry[2].append(user)

    out = ["# Third-party notices", "",
           "dino is MIT licensed (see LICENSE, https://github.com/meetdino/dino). Dino.app and the dino",
           "command line tool include the open source software below, under its own licenses, whose",
           "full texts follow at the end.", "",
           "Written by scripts/third-party.py from packaging/licenses/ and Cargo.lock; don't edit it by hand.",
           "", "## In Dino.app, and in the dino binary where it says so", ""]
    for name, version, source, what, spdx, files, _ in NATIVE:
        label = f"{name} {version}" if version else name
        out.append(f"- {label}, {spdx}. {source}")
        out.append(f"  {what}")
        for text_name, path in files:
            full = os.path.join(LICENSES, path) if path.startswith("native/") else os.path.join(ROOT, path)
            use(text_name, read(full), label)

    # Our own crates are dino's: MIT, LICENSE above.
    crates = [c for c in about["crates"] if c["package"].get("source")]
    ours = {c["package"]["id"] for c in about["crates"] if not c["package"].get("source")}
    out += ["", f"## Rust crates in the dino binary ({len(crates)})", "",
            "The dino binary is the dino command and dinod, in Contents/Helpers of the app and alone",
            "in the CLI tarball.", ""]
    for c in sorted(crates, key=lambda c: (c["package"]["name"].lower(), c["package"]["version"])):
        p = c["package"]
        link = p.get("repository") or f"https://crates.io/crates/{p['name']}"
        out.append(f"- {p['name']} {p['version']}, {c['license']}. {link}")
    for lic in about["licenses"]:
        for u in lic["used_by"]:
            if u["crate"]["id"] in ours:
                continue
            use(lic["name"], lic["text"], f"{u['crate']['name']} {u['crate']['version']}")

    out += ["", "## License texts", ""]
    for name, text, users in sorted(texts.values(), key=lambda e: (e[0].lower(), e[1])):
        out += [f"### {name}", "", "Used by " + ", ".join(sorted(users, key=str.lower)) + ".", "",
                fenced(text), ""]
    return "\n".join(out).rstrip() + "\n"


def check_binary(app):
    exe = os.path.join(app, "Contents", "MacOS", "Dino")
    nm = subprocess.run(["nm", "-gU", exe], capture_output=True, text=True, check=True).stdout
    symbols = [l.split()[-1] for l in nm.splitlines() if l.strip()]
    found = lambda rx: any(re.search(rx, s) for s in symbols)
    problems = []
    for lib, rx in UNLISTED.items():
        if found(rx):
            problems.append(f"{exe} links {lib} (symbols matching {rx}), which NATIVE doesn't list")
    for name, *_, rx in NATIVE:
        if rx:
            print(f"  {name}: {'linked' if found(rx) else 'not linked'}")
    if not found(r"^_ghostty_"):
        problems.append(f"no libghostty symbols in {exe}: is it stripped? The check can't see what it links")
    if problems:
        fail("\n".join(problems))
    print(f"{exe}: every library it links that this script knows of is in THIRD_PARTY_NOTICES.md")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--binary", metavar="APP")
    args = ap.parse_args()
    if args.binary:
        check_binary(args.binary)
        return
    check_packages()
    about = cargo_about()
    text = render(about)
    if args.check:
        if not os.path.exists(OUT) or read(OUT) != text:
            fail("THIRD_PARTY_NOTICES.md is out of date with Cargo.lock or packaging/licenses: "
                 "run scripts/third-party.py and commit the result")
        print("THIRD_PARTY_NOTICES.md is up to date")
        return
    with open(OUT, "w", encoding="utf-8") as f:
        f.write(text)
    print(f"wrote {OUT}: {len([c for c in about['crates'] if c['package'].get('source')])} crates, "
          f"{len(NATIVE)} others")


main()
