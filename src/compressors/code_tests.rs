use super::*;
use crate::cache::{CcrStore, MemoryCcrStore};

#[test]
fn keeps_signatures_collapses_bodies() {
    let mut src = String::from("use std::collections::HashMap;\n\n");
    src.push_str("pub fn process(items: &[i32]) -> i32 {\n");
    for i in 0..30 {
        src.push_str(&format!(
            "        let tmp_{i} = items.iter().sum::<i32>() + {i};\n"
        ));
    }
    src.push_str("        tmp_0\n}\n\n");
    src.push_str("struct Config {\n    name: String,\n    size: usize,\n}\n");
    let out = compress_heuristic(&src).expect("compresses");
    assert!(out.lossy);
    assert!(
        out.text.contains("pub fn process"),
        "signature kept:\n{}",
        out.text
    );
    assert!(out.text.contains("struct Config"));
    assert!(
        out.text.contains("line(s) …"),
        "body collapsed:\n{}",
        out.text
    );
    assert!(
        !out.text.contains("tmp_15"),
        "deep body should be collapsed"
    );
    assert!(out.text.len() < src.len());
}

#[test]
fn short_file_passes_through() {
    let src = "fn a() {}\nfn b() {}\n";
    assert!(compress_heuristic(src).is_none());
}

#[cfg(feature = "tinyjuice-treesitter")]
#[test]
fn treesitter_collapses_rust_body_keeps_struct() {
    let mut src = String::from("use std::collections::HashMap;\n\n");
    src.push_str("pub fn process(items: &[i32]) -> i32 {\n");
    for i in 0..30 {
        src.push_str(&format!(
            "    let tmp_{i} = items.iter().sum::<i32>() + {i};\n"
        ));
    }
    src.push_str("    tmp_0\n}\n\n");
    src.push_str("pub struct Config {\n    pub name: String,\n    pub size: usize,\n}\n");
    let out = treesitter::compress(&src, "rs").expect("compresses");
    assert!(
        out.text.contains("pub fn process(items: &[i32]) -> i32"),
        "{}",
        out.text
    );
    // Struct fields preserved exactly (not a function body).
    assert!(out.text.contains("pub name: String"), "{}", out.text);
    assert!(out.text.contains("pub size: usize"));
    // Function body collapsed.
    assert!(out.text.contains("line(s) …"), "{}", out.text);
    assert!(!out.text.contains("tmp_15"));
    assert!(out.text.len() < src.len());
}

#[cfg(feature = "tinyjuice-treesitter")]
#[test]
fn treesitter_collapses_python_body() {
    let mut src = String::from("import os\n\ndef handler(event):\n");
    for i in 0..30 {
        src.push_str(&format!("    x_{i} = compute(event, {i})\n"));
    }
    src.push_str("    return x_0\n");
    let out = treesitter::compress(&src, "py").expect("compresses");
    assert!(out.text.contains("def handler(event):"), "{}", out.text);
    assert!(out.text.contains("collapsed"), "{}", out.text);
    assert!(!out.text.contains("x_15"));
}

#[cfg(feature = "tinyjuice-treesitter")]
#[test]
fn treesitter_collapses_typescript_body_and_keeps_interface() {
    let mut src = String::from("export interface Config { name: string; size: number; }\n\n");
    src.push_str("export function process(items: number[]): number {\n");
    for i in 0..30 {
        src.push_str(&format!("    const value_{i} = items.length + {i};\n"));
    }
    src.push_str("    return value_0;\n}\n");

    let out = treesitter::compress(&src, "ts").expect("compresses");
    assert!(out.text.contains("export function process(items: number[]): number"));
    assert!(out.text.contains("interface Config"), "{}", out.text);
    assert!(out.text.contains("name: string"), "{}", out.text);
    assert!(out.text.contains("line(s) …"), "{}", out.text);
    assert!(!out.text.contains("value_15"));
}

#[test]
fn keeps_marker_lines_in_body() {
    let mut src = String::from("fn f() {\n");
    for i in 0..20 {
        if i == 10 {
            src.push_str("        // TODO: handle the edge case here\n");
        } else {
            src.push_str(&format!("        do_thing({i});\n"));
        }
    }
    src.push_str("}\n");
    let out = compress_heuristic(&src).expect("compresses");
    assert!(out.text.contains("TODO"), "marker line kept:\n{}", out.text);
}

#[test]
fn stub_code_reports_elisions_and_symbols() {
    let mut src = String::from("use std::fmt;\n\npub fn visible() {\n");
    for i in 0..20 {
        src.push_str(&format!("    println!(\"{i}\");\n"));
    }
    src.push_str("}\n");

    let out = stub_code(&src, Some("rs"), &StubMode::SignaturesOnly, usize::MAX);

    assert_eq!(out.parse_status, ParseStatus::TreeSitter);
    assert!(out.text.contains("pub fn visible()"));
    assert!(out.text.contains("lines"));
    assert!(!out.text.contains("println!(\"10\")"));
    assert_eq!(out.symbols[0].name, "visible");
    assert!(out.symbols[0].public);
    assert_eq!(out.elisions.len(), 1);
    assert!(out.elisions[0].start_line <= out.elisions[0].end_line);
}

#[test]
fn code_stub_transform_requires_retained_ccr() {
    let mut src = String::from("use std::fmt;\n\npub fn visible() {\n");
    for i in 0..20 {
        src.push_str(&format!("    println!(\"{i}\");\n"));
    }
    src.push_str("}\n");
    let pipeline_input = PipelineInput {
        content: &src,
        original_content: &src,
        content_kind: ContentKind::Code,
        original_bytes: src.len(),
    };
    let transform = CodeStubTransform::new(StubMode::SignaturesOnly).with_extension("rs");

    let rejecting_store = MemoryCcrStore::new(1, 1);
    assert!(transform.apply(&pipeline_input, &rejecting_store).is_none());

    let store = MemoryCcrStore::default();
    let out = transform
        .apply(&pipeline_input, &store)
        .expect("retained code stub");

    assert_eq!(out.kind(), CompressorKind::Code);
    assert!(out.text().contains("pub fn visible()"), "{}", out.text());
    assert!(!out.text().contains("println!(\"10\")"), "{}", out.text());
    assert_eq!(store.get(out.token()).as_deref(), Some(src.as_str()));
}

#[test]
fn code_stub_transform_skips_non_code_input() {
    let input = PipelineInput {
        content: "plain text",
        original_content: "plain text",
        content_kind: ContentKind::PlainText,
        original_bytes: "plain text".len(),
    };
    let transform = CodeStubTransform::new(StubMode::SignaturesOnly);
    let store = MemoryCcrStore::default();

    assert_eq!(transform.estimate_bloat(&input), 0.0);
    assert!(transform.apply(&input, &store).is_none());
}

#[test]
fn stub_code_expands_matched_symbol() {
    let mut src = String::from("pub fn visible() {\n");
    for i in 0..10 {
        src.push_str(&format!("    println!(\"visible {i}\");\n"));
    }
    src.push_str("}\n\nfn hidden() {\n");
    for i in 0..10 {
        src.push_str(&format!("    println!(\"hidden {i}\");\n"));
    }
    src.push_str("}\n");

    let out = stub_code(
        &src,
        Some("rs"),
        &StubMode::MatchedSymbols(vec!["visible".to_string()]),
        usize::MAX,
    );

    assert!(out.text.contains("visible 9"), "{}", out.text);
    assert!(!out.text.contains("hidden 9"), "{}", out.text);
    assert_eq!(out.elisions.len(), 1);
}

#[test]
fn stub_code_public_api_omits_private_declarations() {
    let mut src = String::from("use std::fmt;\n\npub fn visible() {\n");
    for i in 0..10 {
        src.push_str(&format!("    println!(\"visible {i}\");\n"));
    }
    src.push_str("}\n\nfn hidden() {\n");
    for i in 0..10 {
        src.push_str(&format!("    println!(\"hidden {i}\");\n"));
    }
    src.push_str("}\n");

    let out = stub_code(&src, Some("rs"), &StubMode::PublicApi, usize::MAX);

    assert!(out.text.contains("use std::fmt;"), "{}", out.text);
    assert!(out.text.contains("pub fn visible()"), "{}", out.text);
    assert!(out.text.contains("private declaration"), "{}", out.text);
    assert!(!out.text.contains("fn hidden"), "{}", out.text);
    assert!(!out.text.contains("hidden 9"), "{}", out.text);
    assert_eq!(out.symbols.len(), 1);
    assert_eq!(out.symbols[0].name, "visible");
    assert!(out.symbols[0].public);
    assert!(
        out.elisions
            .iter()
            .any(|elision| elision.reason == "public_api_private_declaration"),
        "{:?}",
        out.elisions
    );
}

#[test]
fn stub_code_reports_heuristic_fallback_for_unknown_language() {
    let mut src = String::from("function f() {\n");
    for i in 0..12 {
        src.push_str(&format!("  call({i});\n"));
    }
    src.push_str("}\n");

    let out = stub_code(&src, Some("unknown"), &StubMode::SignaturesOnly, usize::MAX);

    assert_eq!(out.parse_status, ParseStatus::HeuristicFallback);
    assert!(!out.elisions.is_empty());
    assert!(out.text.contains("lines"));
}

#[test]
fn stub_code_public_api_heuristic_omits_private_blocks() {
    let mut src = String::from("export function visible() {\n");
    for i in 0..10 {
        src.push_str(&format!("  console.log(\"visible {i}\");\n"));
    }
    src.push_str("}\n\nfunction hidden() {\n");
    for i in 0..10 {
        src.push_str(&format!("  console.log(\"hidden {i}\");\n"));
    }
    src.push_str("}\n");

    let out = stub_code(&src, Some("unknown"), &StubMode::PublicApi, usize::MAX);

    assert_eq!(out.parse_status, ParseStatus::HeuristicFallback);
    assert!(
        out.text.contains("export function visible()"),
        "{}",
        out.text
    );
    assert!(out.text.contains("private declaration"), "{}", out.text);
    assert!(!out.text.contains("function hidden"), "{}", out.text);
    assert!(!out.text.contains("hidden 9"), "{}", out.text);
    assert_eq!(out.symbols.len(), 1);
    assert_eq!(out.symbols[0].name, "visible");
    assert!(out.symbols[0].public);
}

#[test]
fn stub_code_matched_symbol_heuristic_expands_requested_body() {
    let mut src = String::from("function visible() {\n");
    for i in 0..10 {
        src.push_str(&format!("  console.log(\"visible {i}\");\n"));
    }
    src.push_str("}\n\nfunction hidden() {\n");
    for i in 0..10 {
        src.push_str(&format!("  console.log(\"hidden {i}\");\n"));
    }
    src.push_str("}\n");

    let out = stub_code(
        &src,
        Some("unknown"),
        &StubMode::MatchedSymbols(vec!["visible".to_string()]),
        usize::MAX,
    );

    assert_eq!(out.parse_status, ParseStatus::HeuristicFallback);
    assert!(out.text.contains("visible 9"), "{}", out.text);
    assert!(!out.text.contains("hidden 9"), "{}", out.text);
    assert_eq!(out.symbols.len(), 2);
    assert!(
        out.elisions
            .iter()
            .any(|elision| elision.reason.contains("MatchedSymbols")),
        "{:?}",
        out.elisions
    );
}

#[test]
fn stub_code_expand_around_lines_heuristic_expands_containing_body() {
    let mut src = String::from("function first() {\n");
    for i in 0..10 {
        src.push_str(&format!("  console.log(\"first {i}\");\n"));
    }
    src.push_str("}\n\nfunction second() {\n");
    for i in 0..10 {
        src.push_str(&format!("  console.log(\"second {i}\");\n"));
    }
    src.push_str("}\n");

    let out = stub_code(
        &src,
        Some("unknown"),
        &StubMode::ExpandAroundLines(vec![LineRange::new(7, 7)]),
        usize::MAX,
    );

    assert_eq!(out.parse_status, ParseStatus::HeuristicFallback);
    assert!(out.text.contains("first 9"), "{}", out.text);
    assert!(!out.text.contains("second 9"), "{}", out.text);
    assert_eq!(out.symbols.len(), 2);
    assert!(
        out.elisions
            .iter()
            .any(|elision| elision.reason.contains("ExpandAroundLines")),
        "{:?}",
        out.elisions
    );
}

#[test]
fn stub_budget_keeps_complete_lines_and_records_tail_elision() {
    let content = "header\n".to_string() + &"body payload\n".repeat(30);
    let stub = CodeStubOutput {
        text: content.clone(),
        symbols: vec![],
        elisions: vec![],
        parse_status: ParseStatus::HeuristicFallback,
    };
    let out = enforce_stub_budget(stub.clone(), 120);
    assert!(out.text.starts_with("header\n"));
    assert!(out.text.ends_with("[TOKENJUICE CODE STUB TRUNCATED]"));
    assert!(
        out.elisions
            .iter()
            .any(|e| e.reason == "stub_budget" && e.end_line == 31)
    );
    assert_eq!(enforce_stub_budget(stub, 0).text, content);
}

#[test]
fn heuristic_stubs_expand_requested_symbols_and_body_lines() {
    let source = format!(
        "pub fn selected() {{\n{}}}\nfn other() {{\n{}}}",
        "    selected_body();\n".repeat(10),
        "    other_body();\n".repeat(10)
    );
    let matched = stub_heuristic(&source, &StubMode::MatchedSymbols(vec!["selected".into()]));
    assert!(matched.text.contains("selected_body"));
    assert!(!matched.text.contains("other_body"));
    let around = stub_heuristic(
        &source,
        &StubMode::ExpandAroundLines(vec![LineRange::new(5, 5)]),
    );
    assert!(around.text.contains("selected_body"));
    assert!(!around.text.contains("other_body"));
    let around_signature = stub_heuristic(
        &source,
        &StubMode::ExpandAroundLines(vec![LineRange::new(1, 1)]),
    );
    assert!(around_signature.text.contains("selected_body"));
    let empty_match = stub_heuristic(&source, &StubMode::MatchedSymbols(vec![" ".into()]));
    assert!(!empty_match.text.contains("selected_body"));
}

#[test]
fn heuristic_public_api_handles_private_single_lines_and_unclosed_declarations() {
    let source = "fn private() {}\npub fn public() {}\nfn unfinished() {\n    inner();";
    let out = stub_heuristic(source, &StubMode::PublicApi);
    assert!(out.text.contains("pub fn public"));
    assert!(!out.text.contains("fn private") && !out.text.contains("fn unfinished"));
    assert_eq!(out.elisions.len(), 2);
    assert_eq!(out.elisions[1].end_line, 4);
    assert!(symbol_from_signature(" ", 1, 1, "function").is_none());
    assert_eq!(
        symbol_from_signature("unknown", 1, 1, "function")
            .unwrap()
            .name,
        "function"
    );
}

#[test]
fn typed_stub_declines_noncode_and_nonshrinking_input() {
    let transform = CodeStubTransform::new(StubMode::SignaturesOnly).with_extension("rs");
    assert_eq!(transform.mode(), &StubMode::SignaturesOnly);
    let store = MemoryCcrStore::default();
    let input = PipelineInput {
        content: "plain",
        original_content: "plain",
        original_bytes: 5,
        content_kind: ContentKind::PlainText,
    };
    assert_eq!(transform.estimate_bloat(&input), 0.0);
    assert!(transform.apply(&input, &store).is_none());
    let input = PipelineInput {
        content: "fn short() {}",
        original_content: "fn short() {}",
        original_bytes: 13,
        content_kind: ContentKind::Code,
    };
    assert!(transform.apply(&input, &store).is_none());
}

#[test]
fn heuristic_handles_unbalanced_closes_and_short_nested_runs() {
    let source = "}\nfn a() {\n    x();\n}\n".repeat(4);
    let out = compress_heuristic(&source);
    assert_eq!(out.unwrap().text, source.trim_end());
}
