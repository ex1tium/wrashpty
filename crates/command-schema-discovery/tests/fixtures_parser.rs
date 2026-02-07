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
    assert!(schema.find_subcommand("delete").is_some() || schema.find_subcommand("edit").is_some());

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

#[test]
fn test_parse_apt_family_help_outputs() {
    let apt_help = fixture("apt-help.txt");
    let mut apt_parser = HelpParser::new("apt", &apt_help);
    let apt_schema = apt_parser.parse().expect("apt fixture should parse");
    assert_eq!(apt_parser.detected_format(), Some(HelpFormat::Gnu));
    assert_eq!(apt_schema.version.as_deref(), Some("2.8.3"));
    assert!(apt_schema.find_subcommand("install").is_some());
    assert!(apt_schema.find_subcommand("update").is_some());
    assert!(apt_schema.find_subcommand("This").is_none());

    let apt_get_help = fixture("apt-get-help.txt");
    let mut apt_get_parser = HelpParser::new("apt-get", &apt_get_help);
    let apt_get_schema = apt_get_parser
        .parse()
        .expect("apt-get fixture should parse");
    assert_eq!(apt_get_parser.detected_format(), Some(HelpFormat::Gnu));
    assert_eq!(apt_get_schema.version.as_deref(), Some("2.8.3"));
    assert!(apt_get_schema.find_subcommand("install").is_some());
    assert!(
        apt_get_schema
            .positional
            .iter()
            .any(|arg| arg.name == "pkg1")
    );
    assert!(
        apt_get_schema
            .positional
            .iter()
            .any(|arg| arg.name == "pkg2")
    );

    let apt_cache_help = fixture("apt-cache-help.txt");
    let mut apt_cache_parser = HelpParser::new("apt-cache", &apt_cache_help);
    let apt_cache_schema = apt_cache_parser
        .parse()
        .expect("apt-cache fixture should parse");
    assert_eq!(apt_cache_parser.detected_format(), Some(HelpFormat::Gnu));
    assert_eq!(apt_cache_schema.version.as_deref(), Some("2.8.3"));
    assert!(apt_cache_schema.find_subcommand("policy").is_some());
    assert!(apt_cache_schema.find_subcommand("search").is_some());
}

#[test]
fn test_parse_coreutils_and_complex_fixtures() {
    let ls_help = fixture("ls-help.txt");
    let mut ls_parser = HelpParser::new("ls", &ls_help);
    let ls_schema = ls_parser.parse().expect("ls fixture should parse");
    assert!(ls_schema.find_global_flag("--help").is_some());

    let cp_help = fixture("cp-help.txt");
    let mut cp_parser = HelpParser::new("cp", &cp_help);
    let cp_schema = cp_parser.parse().expect("cp fixture should parse");
    assert!(
        cp_schema.find_global_flag("--recursive").is_some()
            || cp_schema.find_global_flag("-r").is_some()
            || cp_schema.find_global_flag("-R").is_some()
    );

    let mv_help = fixture("mv-help.txt");
    let mut mv_parser = HelpParser::new("mv", &mv_help);
    let mv_schema = mv_parser.parse().expect("mv fixture should parse");
    assert!(mv_schema.find_global_flag("--backup").is_some());

    let tar_help = fixture("tar-help.txt");
    let mut tar_parser = HelpParser::new("tar", &tar_help);
    let tar_schema = tar_parser.parse().expect("tar fixture should parse");
    assert!(tar_schema.find_global_flag("--file").is_some());

    let stty_help = fixture("stty-help.txt");
    let mut stty_parser = HelpParser::new("stty", &stty_help);
    let stty_schema = stty_parser.parse().expect("stty fixture should parse");
    assert!(stty_schema.find_subcommand("sane").is_some());

    let node_help = fixture("node-help.txt");
    let mut node_parser = HelpParser::new("node", &node_help);
    let node_schema = node_parser.parse().expect("node fixture should parse");
    assert!(node_schema.find_global_flag("--eval").is_some());
}

#[test]
fn test_no_placeholder_subcommands() {
    let fixtures = [
        ("git", "git-help.txt"),
        ("kubectl", "kubectl-help.txt"),
        ("apt", "apt-help.txt"),
        ("apt-get", "apt-get-help.txt"),
    ];

    for (command, fixture_name) in fixtures {
        let help = fixture(fixture_name);
        let mut parser = HelpParser::new(command, &help);
        let schema = parser.parse().expect("fixture should parse");

        for subcmd in &schema.subcommands {
            assert!(
                !is_placeholder_token(&subcmd.name),
                "Placeholder subcommand '{}' in {}",
                subcmd.name,
                fixture_name
            );
        }
    }
}

#[test]
fn test_no_hierarchy_self_cycles() {
    let fixtures = [
        ("git", "git-help.txt"),
        ("apt", "apt-help.txt"),
        ("stty", "stty-help.txt"),
    ];

    for (command, fixture_name) in fixtures {
        let help = fixture(fixture_name);
        let mut parser = HelpParser::new(command, &help);
        let schema = parser.parse().expect("fixture should parse");

        for subcmd in &schema.subcommands {
            assert_ne!(
                subcmd.name, command,
                "Command '{}' should not contain self-cycle in {}",
                command, fixture_name
            );
            assert!(
                !subcmd
                    .subcommands
                    .iter()
                    .any(|nested| nested.name == subcmd.name),
                "Subcommand '{}' contains itself as nested subcommand in {}",
                subcmd.name,
                fixture_name
            );
        }
    }
}

#[test]
fn test_no_env_vars_as_subcommands() {
    let fixtures = [("node", "node-help.txt"), ("less", "less-help.txt")];

    for (command, fixture_name) in fixtures {
        let help = fixture(fixture_name);
        let mut parser = HelpParser::new(command, &help);
        let schema = parser.parse().expect("fixture should parse");

        for subcmd in &schema.subcommands {
            assert!(
                !looks_like_env_var(&subcmd.name),
                "Environment variable parsed as subcommand '{}' in {}",
                subcmd.name,
                fixture_name
            );
        }
    }
}

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    fs::read_to_string(path).expect("fixture file must be readable")
}

fn is_placeholder_token(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_uppercase().as_str(),
        "COMMAND" | "FILE" | "PATH" | "URL" | "ARG" | "OPTION" | "SUBCOMMAND" | "CMD"
    )
}

fn looks_like_env_var(text: &str) -> bool {
    let Some((key, _)) = text.split_once('=') else {
        return false;
    };

    !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}
