use std::io::Write;
use std::process::{Command, Output, Stdio};

fn invoke(dir: &std::path::Path, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tinyjuice"))
        .args(args)
        .env("TINYJUICE_CCR_DIR", dir.join("ccr"))
        .env("CODEX_HOME", dir.join("codex"))
        .env("CLAUDE_CONFIG_DIR", dir.join("claude"))
        .env("TINYJUICE_MIN_BYTES_TO_COMPRESS", "0")
        .env("TINYJUICE_CCR_ENABLED", "yes")
        .env("TINYJUICE_CCR_TTL_SECS", "60")
        .env("TINYJUICE_LOSSY_WITHOUT_CCR", "invalid")
        .env("TINYJUICE_CODE_TARGET_RATIO", "0.5")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn host_cli_rejects_invalid_arguments_with_useful_errors() {
    let dir = tempfile::tempdir().unwrap();
    let cases: &[(&[&str], &str)] = &[
        (&["unknown"], "unknown command"),
        (&["reduce-json", "--host"], "requires a value"),
        (&["reduce-json", "--unknown"], "unknown reduce-json flag"),
        (&["reduce-json", "a", "b"], "at most one path"),
        (&["install"], "requires a host"),
        (&["install", "--binary"], "requires a value"),
        (&["install", "--path"], "requires a value"),
        (&["install", "--unknown"], "unknown install flag"),
        (&["install", "codex", "claude-code"], "exactly one host"),
        (&["install", "generic-json"], "not implemented"),
        (&["uninstall"], "requires a host"),
        (&["uninstall", "--path"], "requires a value"),
        (&["uninstall", "--unknown"], "unknown uninstall flag"),
        (&["uninstall", "codex", "claude-code"], "exactly one host"),
        (&["retrieve"], "requires a CCR token"),
        (&["retrieve", "a", "b"], "exactly one CCR token"),
        (&["retrieve", "missing"], "no original found"),
        (&["gc", "--ttl-secs"], "requires a value"),
        (&["gc", "--max-bytes"], "requires a value"),
        (&["gc", "--max-bytes=-1"], "non-negative integer"),
        (&["gc", "--unknown"], "unknown gc flag"),
        (&["host-template"], "requires a host"),
        (&["host-template", "--binary"], "requires a value"),
        (
            &["host-template", "--unknown"],
            "unknown host-template flag",
        ),
        (
            &["host-template", "codex", "claude-code"],
            "exactly one host",
        ),
    ];
    for (args, expected) in cases {
        let out = invoke(dir.path(), args, "");
        assert!(!out.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(expected),
            "{args:?}: {out:?}"
        );
    }
    let out = invoke(dir.path(), &["reduce-json"], "{invalid");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid JSON"));
}

#[test]
fn host_cli_help_templates_and_gc_are_usable() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec![],
        vec!["help"],
        vec!["reduce-json", "-h"],
        vec!["install", "-h"],
        vec!["update", "-h"],
        vec!["uninstall", "-h"],
        vec!["gc", "-h"],
        vec!["host-template", "-h"],
    ] {
        let out = invoke(dir.path(), &args, "");
        assert!(out.status.success(), "{args:?}: {out:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("Usage:"));
    }
    let out = invoke(dir.path(), &["hosts"], "");
    assert!(out.status.success());
    assert!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout)
            .unwrap()
            .is_array()
    );
    for args in [
        vec!["host-template", "codex", "--binary", "/custom/tinyjuice"],
        vec!["host-template", "claude-code", "--binary=/custom/tinyjuice"],
    ] {
        let out = invoke(dir.path(), &args, "");
        assert!(out.status.success(), "{out:?}");
        assert!(String::from_utf8_lossy(&out.stdout).contains("/custom/tinyjuice"));
    }
    for args in [
        vec!["gc"],
        vec!["gc", "--ttl-secs", "0", "--max-bytes", "0"],
        vec!["gc", "--ttl-secs=10", "--max-bytes=100"],
    ] {
        let out = invoke(dir.path(), &args, "");
        assert!(out.status.success(), "{out:?}");
        assert!(String::from_utf8_lossy(&out.stdout).contains("removed 0 entries"));
    }
}

#[test]
fn reduce_json_help_and_argument_errors_do_not_open_cache() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("not-a-directory");
    std::fs::write(&cache_path, "not a directory").unwrap();

    for (args, expected_success, expected_output) in [
        (&["reduce-json", "--help"][..], true, "Usage:"),
        (
            &["reduce-json", "--unknown"][..],
            false,
            "unknown reduce-json flag",
        ),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_tinyjuice"))
            .args(args)
            .env("TINYJUICE_CCR_DIR", &cache_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(out.status.success(), expected_success, "{args:?}: {out:?}");
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(output.contains(expected_output), "{args:?}: {output}");
        assert!(!output.contains("not a directory"), "{args:?}: {output}");
    }
}

#[test]
fn retrieve_argument_errors_do_not_open_cache() {
    let dir = tempfile::tempdir().unwrap();
    let cache_path = dir.path().join("not-a-directory");
    std::fs::write(&cache_path, "not a directory").unwrap();

    for (args, expected_error) in [
        (&["retrieve"][..], "retrieve requires a CCR token"),
        (
            &["retrieve", "one", "two"][..],
            "retrieve accepts exactly one CCR token",
        ),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_tinyjuice"))
            .args(args)
            .env("TINYJUICE_CCR_DIR", &cache_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?}: {out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(expected_error), "{args:?}: {stderr}");
        assert!(!stderr.contains("not a directory"), "{args:?}: {stderr}");
    }
}

#[test]
fn host_cli_install_update_and_uninstall_preserve_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, r#"{"custom":"keep"}"#).unwrap();
    let path = path.to_str().unwrap();
    for host in ["codex", "claude-code"] {
        for action in ["install", "update"] {
            let out = invoke(
                dir.path(),
                &[
                    action,
                    host,
                    "--path",
                    path,
                    "--binary",
                    "/custom/tinyjuice",
                    "--no-support-git-signature",
                ],
                "",
            );
            assert!(out.status.success(), "{out:?}");
            let settings: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(settings["custom"], "keep");
            assert!(settings["hooks"]["PostToolUse"].is_array());
        }
        let args = [
            "uninstall".to_string(),
            host.to_string(),
            format!("--path={path}"),
        ];
        let refs: Vec<_> = args.iter().map(String::as_str).collect();
        let out = invoke(dir.path(), &refs, "");
        assert!(out.status.success(), "{out:?}");
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(settings["custom"], "keep");
        let out = invoke(dir.path(), &refs, "");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("no "),
            "{out:?}"
        );
    }
    for host in ["codex", "claude-code"] {
        let out = invoke(dir.path(), &["install", host], "");
        assert!(out.status.success(), "{out:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("not interactive"));
        let out = invoke(
            dir.path(),
            &["install", host, "--support-git-signature"],
            "",
        );
        assert!(out.status.success(), "{out:?}");
        let out = invoke(
            dir.path(),
            &[
                "update",
                host,
                "--support-git-signature",
                "--binary=/custom/tinyjuice",
            ],
            "",
        );
        assert!(out.status.success(), "{out:?}");
        let out = invoke(dir.path(), &["uninstall", host], "");
        assert!(out.status.success(), "{out:?}");
    }
}

#[test]
fn host_cli_reads_json_files_and_accepts_hooks_without_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("request.json");
    std::fs::write(
        &path,
        r#"{"host":"generic-json","toolName":"shell","stdout":"hello"}"#,
    )
    .unwrap();
    for host_arg in [vec!["--host", "codex"], vec!["--host=claude-code"]] {
        let mut args = vec!["reduce-json"];
        args.extend(host_arg);
        args.push(path.to_str().unwrap());
        let out = invoke(dir.path(), &args, "");
        assert!(out.status.success(), "{out:?}");
        assert!(serde_json::from_slice::<serde_json::Value>(&out.stdout).is_ok());
    }
    for host in ["codex-post-tool-use", "claude-code-post-tool-use"] {
        let out = invoke(dir.path(), &[host], "{not json");
        assert!(out.status.success(), "{out:?}");
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn post_tool_hook_skips_compression_when_persistent_cache_storage_cannot_open() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ccr"), "not a directory").unwrap();
    let output = (0..200)
        .map(|index| format!("\tmodified:   src/file_{index}.rs"))
        .collect::<Vec<_>>()
        .join("\n");
    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": "git status"},
        "tool_response": {"output": output, "exitCode": 0}
    })
    .to_string();

    let out = invoke(dir.path(), &["codex-post-tool-use"], &payload);

    assert!(
        out.status.success(),
        "cache setup must not fail the hook: {out:?}"
    );
    assert!(
        out.stdout.is_empty(),
        "without durable recovery, the hook must leave host output unchanged"
    );
}

#[test]
fn host_cli_retrieves_original_from_disk_tier() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("ccr")).unwrap();
    let token = tinyjuice::cache::short_hash("stored original");
    std::fs::write(dir.path().join("ccr").join(&token), "stored original").unwrap();
    let out = invoke(dir.path(), &["retrieve", &token], "");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, b"stored original");
}
