use super::*;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).into()).collect()
}

#[test]
fn commands_report_invalid_flags_missing_values_and_extra_positionals() {
    let cases: &[(&[&str], &str)] = &[
        (&[], "missing command"),
        (&["unknown"], "unknown command"),
        (&["reduce", "--tool-name"], "requires a value"),
        (&["reduce", "--command"], "requires a value"),
        (&["reduce", "--exit-code"], "requires a value"),
        (&["reduce", "--exit-code", "bad"], "must be an integer"),
        (&["reduce", "--max-inline-chars"], "requires a value"),
        (&["reduce", "--max-inline-chars", "-1"], "positive integer"),
        (&["reduce", "--unknown"], "unknown reduce option"),
        (&["reduce", "a", "b"], "at most one path"),
        (&["reduce-json", "--unknown"], "unknown reduce-json option"),
        (&["reduce-json", "a", "b"], "at most one path"),
        (&["discover", "--unknown"], "unknown discover option"),
        (&["discover", "a", "b"], "at most one path"),
        (&["verify", "--unknown"], "unknown verify option"),
        (&["ls"], "requires --store-dir"),
        (&["ls", "--store-dir"], "requires a value"),
        (&["ls", "--unknown"], "unknown ls option"),
        (&["stats"], "requires --store-dir"),
        (&["stats", "--store-dir"], "requires a value"),
        (&["stats", "--unknown"], "unknown stats option"),
        (&["doctor", "--store-dir"], "requires a value"),
        (&["doctor", "--codex-home"], "requires a value"),
        (&["doctor", "--openhuman-root"], "requires a value"),
        (&["doctor", "--unknown"], "unknown doctor option"),
        (&["doctor", "codex", "openhuman"], "at most one host"),
        (&["doctor", "unknown"], "unknown doctor host"),
        (&["install"], "requires a host"),
        (&["install", "--codex-home"], "requires a value"),
        (&["install", "--local"], "requires a binary path"),
        (&["install", "--unknown"], "unknown install option"),
        (&["install", "codex", "other"], "exactly one host"),
        (&["install", "other"], "unknown install host"),
        (&["uninstall"], "requires a host"),
        (
            &["uninstall", "--local", "path"],
            "unknown uninstall option",
        ),
        (&["uninstall", "other"], "unknown uninstall host"),
        (&["cat"], "requires --store-dir"),
        (&["cat", "--store-dir"], "requires a value"),
        (&["cat", "--unknown"], "unknown cat option"),
        (&["cat", "--lines"], "requires START:END"),
        (&["cat", "--bytes"], "requires START:END"),
        (
            &["cat", "--lines", "1:2", "--bytes", "1:2"],
            "at most one range",
        ),
        (
            &["cat", "--bytes", "1:2", "--lines", "1:2"],
            "at most one range",
        ),
        (&["cat", "--store-dir", "dir"], "requires a token"),
        (
            &["cat", "--store-dir", "dir", "invalid"],
            "32-character hex",
        ),
        (&["cat", "a", "b"], "exactly one token"),
        (&["wrap"], "requires --"),
        (&["wrap", "--"], "requires a command"),
        (&["wrap", "--tool-name"], "requires a value"),
        (&["wrap", "--max-inline-chars"], "requires a value"),
        (&["wrap", "--max-inline-chars", "-1"], "positive integer"),
        (&["wrap", "--unknown"], "unknown wrap option"),
        (
            &["wrap", "--", "/nonexistent-tinyjuice-command"],
            "failed to run",
        ),
    ];
    for (values, expected) in cases {
        let error = run(args(values)).expect_err(&format!("{values:?}"));
        assert!(error.contains(expected), "{values:?}: {error}");
    }
}

#[test]
fn help_is_available_for_every_command() {
    assert!(run(args(&["help"])).is_ok());
    for command in [
        "reduce",
        "reduce-json",
        "verify",
        "discover",
        "wrap",
        "ls",
        "cat",
        "stats",
        "doctor",
        "install",
        "uninstall",
    ] {
        assert!(run(args(&[command, "--help"])).is_ok(), "{command}");
    }
}

#[test]
fn file_input_commands_handle_pretty_output_and_malformed_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let text = dir.path().join("output.txt");
    fs::write(&text, "On branch main\n\tmodified:   src/lib.rs\n").unwrap();
    assert!(
        run(args(&[
            "reduce",
            "--tool-name",
            "bash",
            "--command",
            "git status",
            "--exit-code",
            "0",
            "--max-inline-chars",
            "1000",
            text.to_str().unwrap()
        ]))
        .is_ok()
    );
    let payload = dir.path().join("payload.json");
    fs::write(&payload, r#"{"toolName":"shell","stdout":"small"}"#).unwrap();
    assert!(
        run(args(&[
            "reduce-json",
            "--pretty",
            payload.to_str().unwrap()
        ]))
        .is_ok()
    );
    fs::write(&payload, r#"[{"toolName":"shell","stdout":"small"}]"#).unwrap();
    assert!(run(args(&["discover", "--pretty", payload.to_str().unwrap()])).is_ok());
    assert!(parse_discovery_inputs("   ").unwrap().is_empty());
    assert!(
        parse_discovery_inputs("[invalid")
            .unwrap_err()
            .contains("JSON array")
    );
    assert!(
        parse_discovery_inputs("\n{}\ninvalid")
            .unwrap_err()
            .contains("line 2")
    );
    assert!(parse_discovery_inputs("\n{\"toolName\":\"shell\"}\n\n").is_ok());
    assert!(
        run(args(&[
            "reduce",
            dir.path().join("missing").to_str().unwrap()
        ]))
        .unwrap_err()
        .contains("failed to read")
    );
}

#[test]
fn doctor_reports_missing_stores_bad_fixtures_and_broken_host_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    assert!(
        run(args(&[
            "doctor",
            "--no-fixtures",
            "--pretty",
            "--store-dir",
            root
        ]))
        .is_ok()
    );
    let missing = dir.path().join("missing");
    assert!(
        run(args(&["doctor", "--store-dir", missing.to_str().unwrap()]))
            .unwrap_err()
            .contains("broken")
    );
    let fixtures = dir.path().join("fixtures");
    fs::create_dir(&fixtures).unwrap();
    fs::write(fixtures.join("bad.fixture.json"), "{invalid").unwrap();
    assert!(run(args(&["doctor", "--fixtures", fixtures.to_str().unwrap()])).is_err());
    assert!(run(args(&["verify", "--fixtures", fixtures.to_str().unwrap()])).is_err());
    assert!(run(args(&["verify"])).is_ok());
    assert!(
        run(args(&[
            "doctor",
            "hooks",
            "--codex-home",
            root,
            "--openhuman-root",
            root
        ]))
        .is_ok()
    );
    let instruction = codex_instruction_path(dir.path());
    fs::create_dir_all(&instruction).unwrap();
    assert_eq!(doctor_codex(Some(dir.path().into()))["status"], "broken");
    assert!(install_codex(Some(dir.path().into()), None).is_err());
    assert!(uninstall_codex(Some(dir.path().into())).is_err());
    assert!(run(args(&["doctor", "codex", "--codex-home", root, "--pretty"])).is_err());
}

#[test]
fn install_and_uninstall_dispatch_with_explicit_host_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    assert!(
        run(args(&[
            "install",
            "codex",
            "--codex-home",
            root,
            "--local",
            "/custom/tinyjuice"
        ]))
        .is_ok()
    );
    let content = fs::read_to_string(codex_instruction_path(dir.path())).unwrap();
    assert!(content.contains("/custom/tinyjuice reduce-json"));
    assert!(run(args(&["uninstall", "codex", "--codex-home", root])).is_ok());
    assert!(run(args(&["uninstall", "codex", "--codex-home", root])).is_ok());
}

#[test]
fn store_inventory_counts_only_tokens_and_stats_formats_both_modes() {
    let dir = tempfile::tempdir().unwrap();
    let content = "original";
    let token = tinyjuice::cache::short_hash(content);
    fs::write(dir.path().join(token), content).unwrap();
    fs::write(dir.path().join("notes"), "unrelated").unwrap();
    fs::create_dir(dir.path().join("nested")).unwrap();
    let inventory = read_ccr_store_inventory(dir.path()).unwrap();
    assert_eq!(inventory.entries.len(), 1);
    assert_eq!(inventory.ignored_entries, 2);
    assert_eq!(inventory.entries[0].bytes, 8);
    let root = dir.path().to_str().unwrap();
    assert!(run(args(&["stats", "--store-dir", root, "--pretty"])).is_ok());
    assert!(run(args(&["stats", "--store-dir", root])).is_ok());
    assert!(run(args(&["ls", "--store-dir", root])).is_ok());
}

#[test]
fn host_doctor_distinguishes_installed_rust_adapter_and_absent_codex_instructions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src/openhuman/tokenjuice/mod.rs");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "// adapter fixture").unwrap();
    assert_eq!(doctor_openhuman(Some(dir.path().into()))["status"], "ok");
    assert!(
        run(args(&[
            "doctor",
            "openhuman",
            "--openhuman-root",
            dir.path().to_str().unwrap()
        ]))
        .is_ok()
    );
    assert_eq!(
        uninstall_codex(Some(dir.path().into())).unwrap()["changed"],
        false
    );
    let instruction = codex_instruction_path(dir.path());
    fs::create_dir_all(instruction.parent().unwrap()).unwrap();
    fs::write(instruction, "unmanaged instructions").unwrap();
    assert_eq!(doctor_codex(Some(dir.path().into()))["status"], "disabled");
}

#[test]
fn range_parsing_rejects_invalid_numbers_and_reversed_ranges() {
    for raw in ["bad", "x:2", "1:x", "4:2"] {
        assert!(parse_range_arg(raw, "--lines").is_err(), "{raw}");
    }
    assert_eq!(parse_range_arg("2:2", "--bytes").unwrap(), (2, 2));
    assert_eq!(merge_doctor_status("ok", "warn"), "warn");
    assert_eq!(merge_doctor_status("broken", "warn"), "broken");
    assert_eq!(command_binary_status("tinyjuice reduce-json"), None);
    assert_eq!(command_binary_status(""), None);
}
