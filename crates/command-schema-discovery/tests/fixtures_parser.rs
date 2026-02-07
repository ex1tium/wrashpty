use std::fs;
use std::path::PathBuf;

use command_schema_core::HelpFormat;
use command_schema_discovery::parser::HelpParser;

#[test]
fn test_parse_git_fixture_extracts_common_subcommands() {
    let help = fixture("git-help.txt");
    let mut parser = HelpParser::new("git", &help);

    let schema = parser.parse().expect("fixture should parse");
    assert_eq!(parser.detected_format(), Some(HelpFormat::Gnu));

    assert!(schema.find_subcommand("clone").is_some());
    assert!(schema.find_subcommand("status").is_some());
}

#[test]
fn test_parse_kubectl_fixture_extracts_flags_and_subcommands() {
    let help = fixture("kubectl-help.txt");
    let mut parser = HelpParser::new("kubectl", &help);

    let schema = parser.parse().expect("fixture should parse");
    assert_eq!(parser.detected_format(), Some(HelpFormat::Cobra));

    assert!(schema.find_subcommand("get").is_some());
    assert!(schema.find_subcommand("delete").is_some());

    let help_flag = schema
        .global_flags
        .iter()
        .find(|flag| flag.long.as_deref() == Some("--help"));
    assert!(help_flag.is_some());

    let namespace_flag = schema
        .global_flags
        .iter()
        .find(|flag| flag.long.as_deref() == Some("--namespace"));
    assert!(namespace_flag.is_some());
}

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    fs::read_to_string(path).expect("fixture file must be readable")
}
