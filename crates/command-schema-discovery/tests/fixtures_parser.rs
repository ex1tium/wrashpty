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

#[test]
fn test_parse_apt_family_help_outputs() {
    let mut apt_parser = HelpParser::new("apt", APT_HELP_FIXTURE);
    let apt_schema = apt_parser.parse().expect("apt fixture should parse");
    assert_eq!(apt_parser.detected_format(), Some(HelpFormat::Gnu));
    assert_eq!(apt_schema.version.as_deref(), Some("2.8.3"));
    assert!(apt_schema.find_subcommand("install").is_some());
    assert!(apt_schema.find_subcommand("update").is_some());
    assert!(apt_schema.find_subcommand("This").is_none());

    let mut apt_get_parser = HelpParser::new("apt-get", APT_GET_HELP_FIXTURE);
    let apt_get_schema = apt_get_parser.parse().expect("apt-get fixture should parse");
    assert_eq!(apt_get_parser.detected_format(), Some(HelpFormat::Gnu));
    assert_eq!(apt_get_schema.version.as_deref(), Some("2.8.3"));
    assert!(apt_get_schema.find_subcommand("install").is_some());
    assert!(apt_get_schema.positional.iter().any(|arg| arg.name == "pkg1"));
    assert!(apt_get_schema.positional.iter().any(|arg| arg.name == "pkg2"));

    let mut apt_cache_parser = HelpParser::new("apt-cache", APT_CACHE_HELP_FIXTURE);
    let apt_cache_schema = apt_cache_parser
        .parse()
        .expect("apt-cache fixture should parse");
    assert_eq!(apt_cache_parser.detected_format(), Some(HelpFormat::Gnu));
    assert_eq!(apt_cache_schema.version.as_deref(), Some("2.8.3"));
    assert!(apt_cache_schema.find_subcommand("policy").is_some());
    assert!(apt_cache_schema.find_subcommand("search").is_some());
}

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    fs::read_to_string(path).expect("fixture file must be readable")
}

const APT_HELP_FIXTURE: &str = r#"apt 2.8.3 (amd64)
Usage: apt [options] command

Most used commands:
  list - list packages based on package names
  search - search in package descriptions
  show - show package details
  install - install packages
  update - update list of available packages
  upgrade - upgrade the system by installing/upgrading packages

See apt(8) for more information about the available commands.
                                        This APT has Super Cow Powers.
"#;

const APT_GET_HELP_FIXTURE: &str = r#"apt 2.8.3 (amd64)
Usage: apt-get [options] command
       apt-get [options] install|remove pkg1 [pkg2 ...]
       apt-get [options] source pkg1 [pkg2 ...]

Most used commands:
  update - Retrieve new lists of packages
  install - Install new packages
  remove - Remove packages
  source - Download source archives

See apt-get(8) for more information about the available commands.
"#;

const APT_CACHE_HELP_FIXTURE: &str = r#"apt 2.8.3 (amd64)
Usage: apt-cache [options] command
       apt-cache [options] show pkg1 [pkg2 ...]

Most used commands:
  showsrc - Show source records
  search - Search the package list for a regex pattern
  show - Show a readable record for the package
  policy - Show policy settings

See apt-cache(8) for more information about the available commands.
"#;
