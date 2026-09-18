#!/usr/bin/env python3
"""Staged launcher checks; never touches the real user's desktop registration.

Run: python3 packaging/test-desktop.py [--macos-payload BUILD_DIRECTORY]
The optional payload must first be built on macOS with build-desktop.sh.
"""
import argparse
import hashlib
import os
from pathlib import Path
import plistlib
import shlex
import shutil
import subprocess
import tarfile
import tempfile


ROOT = Path(__file__).resolve().parent


def run_helper(payload, env, *args, success=True):
    result = subprocess.run(
        ["bash", str(payload / "install-desktop.sh"), *map(str, args)],
        env=env, capture_output=True, text=True,
    )
    assert (result.returncode == 0) == success, (result.stdout, result.stderr)
    return result


def test_linux(root):
    fake_home = root / "linux home"
    fake_home.mkdir()
    tools = root / "tools"
    tools.mkdir()
    (tools / "uname").write_text("#!/bin/sh\nprintf 'Linux\\n'\n")
    (tools / "uname").chmod(0o755)
    data = fake_home / 'data "quoted" \\ $cash `tick` %field'
    env = dict(os.environ, HOME=str(fake_home), XDG_DATA_HOME=str(data),
               PATH=f"{tools}:{os.environ['PATH']}", HAMSY_DESKTOP_SKIP_REGISTER="1")
    payload = root / "linux package"
    subprocess.run(["bash", str(ROOT / "build-desktop.sh"), str(payload)], env=env, check=True)
    bin_dir = root / 'custom prefix \' " $cash `tick`'
    bin_dir.mkdir()
    binary = bin_dir / "hamsy"
    binary.write_text('#!/bin/sh\nprintf "%s\\0" "$@" > "$HAR_TEST_CAPTURE"\n')
    binary.chmod(0o755)
    capture = root / "arguments"
    env["HAR_TEST_CAPTURE"] = str(capture)
    # Preserve the user's existing defaults even during registration/removal.
    applications = data / "applications"
    applications.mkdir(parents=True)
    defaults = applications / "mimeapps.list"
    defaults.write_text("[Default Applications]\napplication/x-har=charles.desktop;\n")
    defaults_before = defaults.read_bytes()
    run_helper(payload, env, "--binary", binary)
    state = data / "hamsy/desktop"
    desktop = applications / "io.hamsy.har.desktop"
    entry = desktop.read_text()
    assert "Terminal=false" in entry and "NoDisplay=true" in entry
    exec_line = next(line[5:] for line in entry.splitlines() if line.startswith("Exec="))
    # Parse Desktop Entry's backslash layer, then its quoted Exec argv layer.
    decoded = exec_line.replace("\\\\", "\\")
    args = shlex.split(decoded)
    # shlex intentionally preserves backslashes before $/` in double quotes;
    # Desktop Entry's Exec specification requires those escapes to be removed.
    launcher = args[0].replace("\\$", "$").replace("\\`", "`").replace("%%", "%")
    assert launcher == str(state / "launch"), args
    assert args[1:] == ["%F"]
    files = [root / 'first \' " $cash `tick` ;&.har', root / "-second.har"]
    for path in files:
        path.write_text('{"log": {"entries": []}}')
    subprocess.run([launcher, *map(str, files)], env=env, check=True)
    assert capture.read_bytes().split(b"\0") == [b"open", b"--", *[os.fsencode(p) for p in files], b""]
    assert "application/json;" not in entry
    assert defaults.read_bytes() == defaults_before
    # A different CLI prefix must not uninstall the current owner's launcher.
    other = bin_dir / "hamsy-new"
    shutil.copy2(binary, other)
    run_helper(payload, env, "--uninstall", "--binary", other)
    assert desktop.exists()
    run_helper(payload, env, "--binary", other)
    assert (state / "binary-path").read_text().strip() == str(other)
    # Retained installer works after deleting the downloaded payload.
    shutil.rmtree(payload)
    retained = state / "integration"
    run_helper(retained, env, "--binary", other)
    run_helper(retained, env, "--uninstall", "--binary", other)
    assert not state.exists() and not desktop.exists()
    assert defaults.read_bytes() == defaults_before
    # An unrelated launcher must never be overwritten.
    desktop.write_text("unrelated application")
    subprocess.run(["bash", str(ROOT / "build-desktop.sh"), str(payload)], env=env, check=True)
    run_helper(payload, env, "--binary", binary, success=False)
    assert desktop.read_text() == "unrelated application"
    print("Linux packaging, quoting, multi-file handoff, upgrade, ownership, and uninstall checks passed.")


def test_macos(root, payload):
    # A reusable output directory may contain the old CI-built bundle. Only a
    # bundle bearing our ownership identifier may be removed during refresh.
    legacy_info = payload / "macos/Hamsy.app/Contents/Info.plist"
    legacy_info.parent.mkdir(parents=True)
    with legacy_info.open("wb") as stream:
        plistlib.dump({"CFBundleIdentifier": "io.hamsy.har-launcher"}, stream)
    subprocess.run(["bash", str(ROOT / "build-desktop.sh"), str(payload)], check=True)
    assert (payload / "macos/launcher.applescript").is_file()
    assert not (payload / "macos/Hamsy.app").exists()
    fake_home = root / "mac home"
    fake_home.mkdir()
    env = dict(os.environ, HOME=str(fake_home), HAMSY_DESKTOP_SKIP_REGISTER="1")
    binary = root / "hamsy mac"
    binary.write_text("#!/bin/sh\nexit 0\n")
    binary.chmod(0o755)
    run_helper(payload, env, "--binary", binary)
    app = fake_home / "Applications/Hamsy.app"
    state = fake_home / "Library/Application Support/Hamsy/desktop"
    with (app / "Contents/Info.plist").open("rb") as stream:
        plist = plistlib.load(stream)
    assert plist["CFBundleIdentifier"] == "io.hamsy.har-launcher"
    assert plist["CFBundleDocumentTypes"][0]["LSItemContentTypes"] == ["io.hamsy.har"]
    assert plist["UTImportedTypeDeclarations"][0]["UTTypeTagSpecification"]["public.filename-extension"] == ["har"]
    assert (app / "Contents/Resources/Hamsy.icns").exists()
    subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)

    # An icon/compiler failure must leave the currently installed app intact.
    # This exercises the failure after osacompile/PlistBuddy, before the final
    # replacement copy removes the old bundle.
    before = {
        path.relative_to(app): path.read_bytes()
        for path in app.rglob("*") if path.is_file()
    }
    icon = payload / "hamsy.png"
    icon_bytes = icon.read_bytes()
    icon.write_bytes(b"not a PNG")
    run_helper(payload, env, "--binary", binary, success=False)
    after = {
        path.relative_to(app): path.read_bytes()
        for path in app.rglob("*") if path.is_file()
    }
    assert after == before
    icon.write_bytes(icon_bytes)

    # A failure during the final same-filesystem rename must also restore the
    # previous app. The shim fails only once, so the rollback rename succeeds.
    mv_tools = root / "mv failure tools"
    mv_tools.mkdir()
    marker = root / "mv failed once"
    (mv_tools / "mv").write_text(
        "#!/bin/sh\n"
        "destination=\"\"\n"
        "for arg do destination=\"$arg\"; done\n"
        "case \"$destination\" in\n"
        "  */Applications/Hamsy.app)\n"
        "    if [ ! -e \"$HAMSY_MV_FAILURE_MARKER\" ]; then\n"
        "      /usr/bin/touch \"$HAMSY_MV_FAILURE_MARKER\"\n"
        "      exit 1\n"
        "    fi\n"
        "    ;;\n"
        "esac\n"
        "exec /bin/mv \"$@\"\n"
    )
    (mv_tools / "mv").chmod(0o755)
    atomic_failure_env = dict(
        env,
        PATH=f"{mv_tools}:{env['PATH']}",
        HAMSY_MV_FAILURE_MARKER=str(marker),
    )
    run_helper(payload, atomic_failure_env, "--binary", binary, success=False)
    after_rename_failure = {
        path.relative_to(app): path.read_bytes()
        for path in app.rglob("*") if path.is_file()
    }
    assert after_rename_failure == before

    # If rollback itself fails, the backup must be retained and reported so a
    # user can recover the previous launcher instead of losing it.
    double_mv_tools = root / "double mv failure tools"
    double_mv_tools.mkdir()
    double_marker = root / "double mv failed"
    (double_mv_tools / "mv").write_text(
        "#!/bin/sh\n"
        "destination=\"\"\n"
        "for arg do destination=\"$arg\"; done\n"
        "case \"$destination\" in\n"
        "  */Applications/Hamsy.app)\n"
        "    count=0\n"
        "    [ -e \"$HAMSY_MV_FAILURE_MARKER\" ] && count=$(cat \"$HAMSY_MV_FAILURE_MARKER\")\n"
        "    if [ \"$count\" -lt 2 ]; then\n"
        "      count=$((count + 1))\n"
        "      printf '%s\\n' \"$count\" > \"$HAMSY_MV_FAILURE_MARKER\"\n"
        "      exit 1\n"
        "    fi\n"
        "    ;;\n"
        "esac\n"
        "exec /bin/mv \"$@\"\n"
    )
    (double_mv_tools / "mv").chmod(0o755)
    double_failure_env = dict(
        env,
        PATH=f"{double_mv_tools}:{env['PATH']}",
        HAMSY_MV_FAILURE_MARKER=str(double_marker),
    )
    result = run_helper(payload, double_failure_env, "--binary", binary, success=False)
    preserved_line = next(
        line for line in result.stderr.splitlines()
        if "previous launcher was preserved at " in line
    )
    preserved = Path(preserved_line.rsplit(" at ", 1)[1])
    assert preserved.is_dir()
    preserved_files = {
        path.relative_to(preserved): path.read_bytes()
        for path in preserved.rglob("*") if path.is_file()
    }
    assert preserved_files == before

    run_helper(payload, env, "--binary", binary)
    run_helper(state / "integration", env, "--uninstall", "--binary", binary)
    assert not state.exists() and not app.exists()
    app.mkdir(parents=True)
    (app / "unrelated").write_text("keep")
    run_helper(payload, env, "--binary", binary, success=False)
    assert (app / "unrelated").read_text() == "keep"
    print("macOS staged installation, HAR type/icon, signature, atomic failure preservation, refresh, ownership, and uninstall checks passed.")


def test_installers(root):
    fixture = root / "installer fixture"
    fixture.mkdir()
    tools = fixture / "tools"
    tools.mkdir()
    (tools / "uname").write_text('#!/bin/sh\ncase "$1" in -m) echo x86_64;; *) echo Linux;; esac\n')
    binary = fixture / "hamsy"
    binary.write_text('#!/bin/sh\n[ "$1" != --version ] || echo "hamsy test"\nexit 0\n')
    binary.chmod(0o755)
    env = dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}", HAMSY_DESKTOP_SKIP_REGISTER="1")
    (tools / "uname").chmod(0o755)
    payload = fixture / "packaging"
    subprocess.run(["bash", str(ROOT / "build-desktop.sh"), str(payload)], env=env, check=True)
    archive = fixture / "hamsy-x86_64-unknown-linux-gnu.tar.gz"
    sums = fixture / "sha256sums.txt"
    # Fake just transport, keeping the actual checksum/extraction/install logic.
    (tools / "curl").write_text('''#!/bin/bash
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) output="$2"; shift 2 ;;
    https://*) url="$1"; shift ;;
    *) shift ;;
  esac
done
case "$url" in
  */sha256sums.txt) cp "$HAR_TEST_SUMS" "$output" ;;
  *) cp "$HAR_TEST_ARCHIVE" "$output" ;;
esac
printf '200'
''')
    (tools / "curl").chmod(0o755)
    env.update(HAR_TEST_ARCHIVE=str(archive), HAR_TEST_SUMS=str(sums), HAR_TEST_BINARY=str(binary))
    compat = subprocess.run(["bash", str(ROOT.parent / "install_source.sh"), "--help"], capture_output=True, text=True)
    assert compat.returncode == 0 and "Usage: install.sh" in compat.stdout

    dispatch = fixture / "dispatch"
    dispatch.mkdir()
    shutil.copy2(ROOT.parent / "install.sh", dispatch / "install.sh")
    source_stub = dispatch / "install-from-source.sh"
    source_stub.write_text("#!/bin/sh\nprintf 'source dispatch ok\\n'\n")
    source_stub.chmod(0o755)
    result = subprocess.run(["bash", str(dispatch / "install.sh"), "--from-source"], capture_output=True, text=True)
    assert result.returncode == 0 and result.stdout == "source dispatch ok\n", (result.stdout, result.stderr)

    # Checksum failures must happen before an existing binary is replaced.
    with tarfile.open(archive, "w:gz") as tar:
        tar.add(binary, arcname="hamsy")
    for failure, sums_line in [("missing", ""), ("corrupt", "0" * 64 + "  ./" + archive.name + "\n")]:
        sums.write_text(sums_line)
        case_dir = fixture / f"checksum-{failure}"
        case_dir.mkdir()
        destination = case_dir / "custom bin"
        destination.mkdir()
        existing = destination / "hamsy"
        existing.write_text("existing binary must survive\n")
        existing.chmod(0o755)
        case_env = dict(env, HOME=str(case_dir), XDG_DATA_HOME=str(case_dir / "data"))
        result = subprocess.run(
            ["bash", str(ROOT.parent / "install.sh"), "--prefix", str(destination), "--no-cert", "--no-path", "--no-desktop"],
            env=case_env, capture_output=True, text=True,
        )
        assert result.returncode != 0
        assert existing.read_text() == "existing binary must survive\n"

    for with_desktop, opt_out in [(True, False), (True, True), (False, False)]:
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(binary, arcname="hamsy")
            if with_desktop:
                tar.add(payload, arcname="packaging")
        sums.write_text(f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  ./{archive.name}\n")
        case_dir = fixture / f"prebuilt-{with_desktop}-{opt_out}"
        case_dir.mkdir()
        case_env = dict(env, HOME=str(case_dir), XDG_DATA_HOME=str(case_dir / "data"))
        args = ["bash", str(ROOT.parent / "install.sh"), "--prefix", str(case_dir / "custom bin"), "--no-cert", "--no-path"]
        if opt_out:
            args.append("--no-desktop")
        result = subprocess.run(args, env=case_env, capture_output=True, text=True)
        assert result.returncode == 0, (result.stdout, result.stderr)
        assert (case_dir / "custom bin/hamsy").exists()
        assert (case_dir / "data/applications/io.hamsy.har.desktop").exists() == (with_desktop and not opt_out)
        if with_desktop and not opt_out:
            user_data = case_dir / ".hamsy"
            user_data.mkdir()
            (user_data / "settings.toml").write_text("keep me\n")
            uninstall = subprocess.run(
                ["bash", str(ROOT.parent / "install.sh"), "--prefix", str(case_dir / "custom bin"), "--uninstall"],
                env=case_env, input="", capture_output=True, text=True,
            )
            assert uninstall.returncode == 0, (uninstall.stdout, uninstall.stderr)
            assert not (case_dir / "custom bin/hamsy").exists()
            assert not (case_dir / "data/applications/io.hamsy.har.desktop").exists()
            assert (user_data / "settings.toml").read_text() == "keep me\n"

    checkout = fixture / "checkout"
    checkout.mkdir()
    shutil.copy2(ROOT.parent / "install.sh", checkout)
    shutil.copy2(ROOT.parent / "install-from-source.sh", checkout)
    shutil.copytree(ROOT, checkout / "packaging")
    (checkout / "Cargo.toml").touch()
    (checkout / "ui").mkdir()
    (tools / "cargo").write_text('''#!/bin/sh
if [ "$1" = --version ]; then echo 'cargo test'; exit 0; fi
mkdir -p target/release
cp "$HAR_TEST_BINARY" target/release/hamsy
''')
    (tools / "node").write_text("#!/bin/sh\necho v22.0.0\n")
    (tools / "pnpm").write_text("#!/bin/sh\nexit 0\n")
    for name in ["cargo", "node", "pnpm"]:
        (tools / name).chmod(0o755)
    for flag in ["--no-ui", "--no-desktop"]:
        case_dir = fixture / f"source{flag}"
        case_dir.mkdir()
        case_env = dict(env, HOME=str(case_dir), XDG_DATA_HOME=str(case_dir / "data"))
        args = ["bash", str(checkout / "install.sh"), "--from-source", "--prefix", str(case_dir / "custom bin"), "--no-cert", "--no-path", "--skip-deps", flag]
        result = subprocess.run(args, env=case_env, capture_output=True, text=True)
        assert result.returncode == 0, (result.stdout, result.stderr)
        assert (case_dir / "custom bin/hamsy").exists()
        assert not (case_dir / "data/applications/io.hamsy.har.desktop").exists()
    print("Prebuilt installer integration/opt-out/legacy archive and source installer no-UI/opt-out checks passed.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--macos-payload", type=Path)
    options = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="hamsy-desktop-test-") as directory:
        root = Path(directory)
        test_linux(root)
        test_installers(root)
        if options.macos_payload:
            test_macos(root, options.macos_payload.resolve())
