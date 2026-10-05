# Sourced by app/build.sh and scripts/release.sh: the parts of Dino.app both make the same way.

# The build a commit makes, for `dino --version`, dinod and the app's Info.plist (DinoBuild): the
# commit, and with uncommitted changes (new source files too) a digest of them, so two such builds differ
# only if they do.
dino_build_id() {
    local root="$1" id changes
    id="$(git -C "$root" rev-parse --short=9 HEAD 2>/dev/null)" || return 0
    # New files only where the code is: not a stray log or pid file in the checkout.
    local new
    new="$(git -C "$root" ls-files -o --exclude-standard -- crates app scripts)"
    changes="$(git -C "$root" diff HEAD --; [ -z "$new" ] || (cd "$root" && printf '%s\n' "$new" | git hash-object --stdin-paths | paste - <(printf '%s\n' "$new")))"
    if [ -n "$changes" ]; then id="$id-dirty.$(printf %s "$changes" | shasum | cut -c1-6)"; fi
    echo "$id"
}

# dinod as the app's launch agent, registered with SMAppService (app/Sources/Dino/LaunchAgent.swift):
# what it runs then has the permissions given to the app. Restarted by launchd if it crashes, not
# otherwise (`dino stop` stays stopped), and not started at login: dino starts it when used, as before.
# Started again 2 s after it last started at the soonest, not launchd's 10: `dino stop` then a start.
#   dino_agent APP BUNDLE_ID [AGENT_HOME]
# AGENT_HOME: the $DINO_HOME of a second, isolated dino, which the app then runs with too, however
# it's opened (Finder, or relaunched by an update).
dino_agent() {
    local app="$1" bundle_id="$2" agent_home="${3:-}" label="$2.dinod" env=""
    if [ -n "$agent_home" ]; then
        label="$label.$(printf %s "$agent_home" | shasum -a 256 | cut -c1-8)"
        env="<key>DINO_HOME</key><string>$agent_home</string>"
        /usr/libexec/PlistBuddy -c "Add :DinoHome string $agent_home" "$app/Contents/Info.plist"
    fi
    mkdir -p "$app/Contents/Library/LaunchAgents"
    cat > "$app/Contents/Library/LaunchAgents/$label.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>$label</string>
    <key>BundleProgram</key><string>Contents/Helpers/dino</string>
    <key>ProgramArguments</key><array><string>dino</string><string>daemon</string></array>
    <key>AssociatedBundleIdentifiers</key><array><string>$bundle_id</string></array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>DINO_LAUNCHD</key><string>$label</string>$env
    </dict>
    <key>KeepAlive</key><dict><key>Crashed</key><true/></dict>
    <key>ThrottleInterval</key><integer>2</integer>
    <key>ProcessType</key><string>Interactive</string>
    <key>AbandonProcessGroup</key><true/>
</dict>
</plist>
PLIST
    plutil -lint -s "$app/Contents/Library/LaunchAgents/$label.plist"
}

# Signing: inside out, never --deep (Apple's guidance), always with the hardened runtime (no
# DYLD_INSERT_LIBRARIES or other injection into dino, which holds your folder and automation
# grants). IDENTITY "-" signs ad hoc; TIMESTAMP is --timestamp for a release, --timestamp=none
# otherwise (no network). An ad hoc signature has no Team ID for the framework to share with the
# app: see app/AdHoc.entitlements.
#   dino_sign APP IDENTITY TIMESTAMP ENTITLEMENTS
dino_sign() {
    local app="$1" identity="$2" timestamp="$3" entitlements="$4" f
    sign() {
        if [ "$identity" = - ]; then
            codesign --force --options runtime --sign - "$@"
        else
            codesign --force --options runtime "$timestamp" --sign "$identity" "$@"
        fi
    }
    while IFS= read -r -d '' f; do
        if file -b "$f" | grep -q 'Mach-O'; then sign "$f"; fi
    done < <(find "$app/Contents/Resources" -type f -print0)
    # Sparkle, inside out, as its documentation lists.
    local spk="$app/Contents/Frameworks/Sparkle.framework/Versions/B"
    sign "$spk/XPCServices/Installer.xpc"
    sign --preserve-metadata=entitlements "$spk/XPCServices/Downloader.xpc"
    sign "$spk/Autoupdate"
    sign "$spk/Updater.app"
    sign "$app/Contents/Frameworks/Sparkle.framework"
    if [ -e "$app/Contents/Helpers/dino" ]; then sign "$app/Contents/Helpers/dino"; fi
    if [ "$identity" = - ]; then
        sign --entitlements "$entitlements" "$app"
    else
        sign "$app"
    fi
    codesign --verify --strict --verbose=1 "$app"
}
