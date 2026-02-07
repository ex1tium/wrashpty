use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use command_schema_core::{CommandSchema, SchemaPackage, validate_package, validate_schema};

fn main() {
    let schema_dir = Path::new("schemas/curated");
    println!("cargo:rerun-if-changed={}", schema_dir.display());

    if !schema_dir.exists() {
        panic!(
            "Missing curated schema directory '{}'. Create it and add schema JSON files.",
            schema_dir.display()
        );
    }

    let schema_paths = collect_schema_paths(schema_dir)
        .unwrap_or_else(|err| panic!("Failed to collect schema files: {err}"));

    if schema_paths.is_empty() {
        panic!(
            "No curated schema JSON files found in '{}'.",
            schema_dir.display()
        );
    }

    for path in &schema_paths {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let mut schemas = Vec::with_capacity(schema_paths.len());

    for path in &schema_paths {
        let raw = fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("Failed to read '{}': {err}", path.display()));

        let schema: CommandSchema = serde_json::from_str(&raw)
            .unwrap_or_else(|err| panic!("Invalid schema JSON '{}': {err}", path.display()));

        if let Some(first_error) = validate_schema(&schema).into_iter().next() {
            panic!(
                "Schema validation failed for '{}': {first_error}",
                path.display()
            );
        }

        schemas.push(schema);
    }

    schemas.sort_by(|left, right| left.command.cmp(&right.command));

    let generated_at = current_unix_timestamp();
    let mut package = SchemaPackage::new(env!("CARGO_PKG_VERSION"), generated_at.clone());
    package.name = Some("wrashpty-curated".to_string());
    package.description = Some("Curated command schemas embedded in wrashpty".to_string());
    package.schemas = schemas;

    if let Some(first_error) = validate_package(&package).into_iter().next() {
        panic!("Schema package validation failed: {first_error}");
    }

    let hash_input = serde_json::to_vec(&package.schemas)
        .unwrap_or_else(|err| panic!("Failed to serialize schemas for hash input: {err}"));
    let bundle_hash = format!("{:016x}", fnv1a64(&hash_input));
    let schema_count = package.schemas.len();

    package.bundle_hash = Some(bundle_hash.clone());

    let bundled_raw = serde_json::to_string(&package)
        .unwrap_or_else(|err| panic!("Failed to serialize bundled schema package: {err}"));

    let out_dir =
        PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR environment variable must be set"));

    fs::write(out_dir.join("embedded_schemas.json"), bundled_raw)
        .unwrap_or_else(|err| panic!("Failed to write embedded_schemas.json: {err}"));

    let schema_meta = format!(
        "pub const EMBEDDED_SCHEMA_BUNDLE_VERSION: &str = {:?};\n\
         pub const EMBEDDED_SCHEMA_BUNDLE_HASH: &str = {:?};\n\
         pub const EMBEDDED_SCHEMA_BUNDLE_GENERATED_AT: &str = {:?};\n\
         pub const EMBEDDED_SCHEMA_COUNT: usize = {};\n",
        package.version, bundle_hash, generated_at, schema_count
    );

    fs::write(out_dir.join("schema_meta.rs"), schema_meta)
        .unwrap_or_else(|err| panic!("Failed to write schema_meta.rs: {err}"));
}

fn collect_schema_paths(dir: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut paths = Vec::new();

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().is_some_and(|ext| ext == "json") {
            paths.push(path);
        }
    }

    paths.sort();
    Ok(paths)
}

fn current_unix_timestamp() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs().to_string(),
        Err(_) => "0".to_string(),
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100000001B3;

    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }

    hash
}
