//! Adapter contract tests use executable stubs, never the host package tools.
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

struct Host {
    dir: tempfile::TempDir,
}

impl Host {
    fn new(id: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("os-release"), format!("ID={id}\n")).unwrap();
        for tool in [
            "dnf",
            "rpm",
            "pacman",
            "apt",
            "apt-cache",
            "zypper",
            "apk",
            "nix-env",
            "nix-channel",
            "xbps-install",
            "xbps-query",
            "emerge",
            "qlist",
        ] {
            let script = format!(
                "#!/bin/sh\nprintf '%s\\n' '{tool}' \"$@\" >> \"$ARCHON_TEST_LOG\"\nprintf '%s' \"$ARCHON_TEST_STDOUT\"\nexit \"${{ARCHON_TEST_EXIT:-0}}\"\n"
            );
            executable(&dir.path().join(tool), &script);
        }
        executable(
            &dir.path().join("sudo"),
            "#!/bin/sh\nprintf '%s\\n' sudo >> \"$ARCHON_TEST_LOG\"\nif [ \"$1\" = -- ]; then shift; fi\nexec \"$@\"\n",
        );
        Self { dir }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_archon"));
        command
            .args(args)
            .env("ARCHON_OS_RELEASE", self.dir.path().join("os-release"))
            // Only our stubs are resolvable. A missing stub fails instead of
            // falling back to a real host package manager.
            .env("PATH", self.dir.path())
            .env("ARCHON_TEST_LOG", self.dir.path().join("calls"))
            .env("ARCHON_TEST_STDOUT", "")
            .env("ARCHON_TEST_EXIT", "0")
            .stdin(Stdio::null());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("calls")).unwrap_or_default()
    }
}

fn executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn assert_ok(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn upgrade_dry_run_is_isolated_for_every_family() {
    for (id, expected) in [
        ("arch", "pacman -Syu --noconfirm"),
        ("ubuntu", "apt upgrade -y"),
        ("fedora", "dnf --refresh upgrade -y"),
        ("opensuse", "zypper --non-interactive update"),
        ("alpine", "apk upgrade --update-cache"),
        ("nixos", "nix-env --upgrade"),
        ("void", "xbps-install -Su -y"),
        ("gentoo", "emerge --update --deep --newuse @world"),
    ] {
        let host = Host::new(id);
        let output = host.run(&["upgrade", "--dry-run", "--yes"]);
        assert_ok(&output);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains(expected), "{id}: {stdout}");
        assert!(
            host.calls().is_empty(),
            "{id} executed a native tool in dry-run"
        );
    }
}

#[test]
fn query_operations_never_escalate_on_any_family() {
    for id in [
        "arch", "debian", "fedora", "opensuse", "alpine", "nixos", "void", "gentoo",
    ] {
        let host = Host::new(id);
        assert_ok(&host.run(&["info", "bash"]));
        assert_ok(&host.run(&["list"]));
        assert!(
            !host.calls().lines().any(|line| line == "sudo"),
            "{id} escalated a query"
        );
        assert!(
            !host.calls().contains("-y\n"),
            "{id} passed confirmation options to a query"
        );
    }
}

#[test]
fn fedora_operations_execute_only_the_requested_native_action() {
    for (args, expected) in [
        (vec!["upgrade", "--yes"], "dnf\n--refresh\nupgrade\n-y\n"),
        (vec!["refresh", "--yes"], "dnf\n--refresh\nmakecache\n"),
        (
            vec!["reinstall", "bash", "--yes"],
            "dnf\nreinstall\n-y\nbash\n",
        ),
        (vec!["repos"], "dnf\nrepolist\n--all\n"),
        (vec!["check"], "dnf\ncheck\n"),
        (vec!["history"], "dnf\nhistory\nlist\n"),
        (
            vec!["provides", "*/libexample.so"],
            "dnf\nprovides\n*/libexample.so\n",
        ),
    ] {
        let host = Host::new("fedora");
        assert_ok(&host.run(&args));
        let calls = host.calls();
        assert!(calls.ends_with(expected), "{args:?}: {calls}");
        assert!(!calls.contains("pacman"));
    }
}

#[test]
fn native_failure_is_propagated_without_a_fallback_backend() {
    let host = Host::new("fedora");
    let output = host
        .command(&["upgrade", "--yes"])
        .env("ARCHON_TEST_EXIT", "42")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("42"));
    assert!(!host.calls().contains("pacman"));
}

#[test]
fn refusing_confirmation_prevents_native_execution() {
    let host = Host::new("fedora");
    let mut child = host
        .command(&["upgrade"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"n\n").unwrap();
    assert_ok(&child.wait_with_output().unwrap());
    assert!(host.calls().is_empty());
}

#[test]
fn nix_profile_mutations_never_use_sudo() {
    let host = Host::new("nixos");
    assert_ok(&host.run(&["upgrade", "--yes"]));
    assert_ok(&host.run(&["refresh", "--yes"]));
    assert_eq!(host.calls(), "nix-env\n--upgrade\nnix-channel\n--update\n");
}

#[test]
fn unsupported_operations_and_invalid_targets_stop_before_execution() {
    for (id, args) in [
        ("arch", vec!["refresh", "--yes"]),
        ("debian", vec!["history"]),
        ("nixos", vec!["reinstall", "bash", "--yes"]),
        ("fedora", vec!["reinstall", "--", "--nogpgcheck"]),
        ("fedora", vec!["info", "--", "--nogpgcheck"]),
        ("fedora", vec!["provides", "--", "--nogpgcheck"]),
        ("fedora", vec!["upgrade", "bash", "--yes"]),
    ] {
        let host = Host::new(id);
        assert!(!host.run(&args).status.success(), "{id}: {args:?}");
        assert!(host.calls().is_empty());
    }
}

#[test]
fn rpm_json_inventory_is_structured_and_stdout_has_no_banners() {
    for id in ["fedora", "opensuse"] {
        let host = Host::new(id);
        let output = host
            .command(&["list", "--json"])
            .env("ARCHON_TEST_STDOUT", "bash\t2:5.3-1.fc43\tx86_64\n")
            .output()
            .unwrap();
        assert_ok(&output);
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value[0]["name"], "bash");
        assert_eq!(value[0]["version"], "2:5.3-1.fc43");
        assert_eq!(value[0]["architecture"], "x86_64");
        assert!(host.calls().starts_with("rpm\n-qa\n--queryformat\n"));
        assert!(!host.calls().contains("sudo"));
    }
}

#[test]
fn inventory_never_turns_a_native_or_parse_error_into_an_empty_success() {
    let host = Host::new("fedora");
    for (stdout, code) in [("broken record\n", "0"), ("", "9")] {
        let output = host
            .command(&["list", "--json"])
            .env("ARCHON_TEST_STDOUT", stdout)
            .env("ARCHON_TEST_EXIT", code)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(error["event"], "error");
        assert_eq!(error["schema_version"], 1);
    }
    let other = Host::new("arch");
    assert!(!other.run(&["list", "--json"]).status.success());
    assert!(other.calls().is_empty());
}

#[test]
fn capabilities_do_not_invoke_tools_and_are_adapter_specific() {
    let fedora = Host::new("fedora");
    let output = fedora.run(&["capabilities"]);
    assert_ok(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("history: supported"));
    assert!(fedora.calls().is_empty());
    let arch = Host::new("arch");
    let output = arch.run(&["capabilities"]);
    assert_ok(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("history: unavailable"));
    assert!(arch.calls().is_empty());
}

#[test]
fn native_transaction_extension_does_not_opt_other_adapters_in() {
    for id in [
        "arch", "debian", "opensuse", "alpine", "nixos", "void", "gentoo",
    ] {
        let host = Host::new(id);
        let output = host.run(&["transaction", "plan", "install", "bash", "--json"]);
        assert!(!output.status.success());
        let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(error["event"], "error");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not yet supported")
        );
        assert!(host.calls().is_empty());
    }
}

#[test]
fn invalid_json_cli_requests_return_versioned_errors_without_native_execution() {
    let host = Host::new("fedora");
    for args in [
        vec!["transaction", "plan", "unknown", "--json"],
        vec!["transaction", "plan", "upgrade", "bash", "--json"],
    ] {
        let output = host.run(&args);
        assert!(!output.status.success());
        let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(error["schema_version"], 1);
        assert_eq!(error["event"], "error");
        assert!(error["error"]["code"].is_string());
        assert!(host.calls().is_empty());
    }
}
