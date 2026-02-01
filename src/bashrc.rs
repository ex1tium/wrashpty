//! Generated bashrc with OSC 777 markers.
//!
//! This module generates a temporary bashrc file that injects PROMPT_COMMAND
//! hooks to emit OSC 777 markers for precmd/prompt/preexec events.
//!
//! # Marker Protocol
//!
//! The bashrc emits three types of markers:
//! - `PRECMD;{exit_code}` - Emitted before prompt display, includes last exit code
//! - `PROMPT` - Emitted within PS1 to mark prompt boundary
//! - `PREEXEC` - Emitted just before command execution via DEBUG trap
//!
//! All markers use the OSC 777 format: `\e]777;{token};{type}[;{data}]\a`
//! where `{token}` is a session-unique 16-character hex string.

use std::io::Write;

use anyhow::{Context, Result};
use tempfile::NamedTempFile;

/// Generates a temporary bashrc file with OSC 777 marker hooks.
///
/// Creates a temporary file containing bash initialization that:
/// - Sources the user's existing `~/.bashrc` if present
/// - Sets up `PROMPT_COMMAND` to emit `PRECMD` markers with exit code
/// - Prepends `PROMPT` marker to `PS1`
/// - Installs a DEBUG trap to emit `PREEXEC` markers before command execution
///
/// # Returns
///
/// A tuple of `(bashrc_path, session_token)` where:
/// - `bashrc_path` is the path to the temporary bashrc file
/// - `session_token` is the 16-byte hex token for marker validation
///
/// # Errors
///
/// Returns an error if:
/// - Random number generation fails
/// - Temporary file creation fails
/// - Writing to the file fails
///
/// # Example
///
/// ```ignore
/// let (bashrc_path, token) = bashrc::generate()?;
/// let pty = Pty::spawn(&bashrc_path, 80, 24)?;
/// let pump = Pump::new(pty.master_fd(), token, None);
/// ```
pub fn generate() -> Result<(String, [u8; 16])> {
    // Generate cryptographically secure 8-byte session token
    let mut token_bytes = [0u8; 8];
    getrandom::getrandom(&mut token_bytes)
        .map_err(|e| anyhow::anyhow!("Failed to generate session token: {}", e))?;

    // Convert to 16-character hex string
    let token_hex = bytes_to_hex_string(&token_bytes);

    // Convert hex string to [u8; 16] for marker parser
    let mut session_token = [0u8; 16];
    session_token.copy_from_slice(token_hex.as_bytes());

    // Create temporary file that persists after function returns
    let mut file = NamedTempFile::new().context("Failed to create temporary bashrc file")?;

    // Write bashrc content
    let bashrc_content = format!(
        r##"# Wrashpty generated bashrc - DO NOT EDIT
# This file is auto-generated and will be deleted on exit

# Session token for marker validation
__wrash_token='{token}'

# Source user's existing bashrc if present
if [[ -f ~/.bashrc ]]; then
    source ~/.bashrc
fi

# Preserve user's existing PROMPT_COMMAND
__user_prompt_command="${{PROMPT_COMMAND:-}}"

# Precmd function emits PRECMD marker with exit code
__wrash_precmd() {{
    local ec=$?
    # Execute user's original PROMPT_COMMAND
    if [[ -n "$__user_prompt_command" ]]; then
        eval "$__user_prompt_command"
    fi
    # Emit PRECMD marker with exit code
    printf '\e]777;%s;PRECMD;%d\a' "$__wrash_token" "$ec"
}}

# Set our precmd as PROMPT_COMMAND
PROMPT_COMMAND='__wrash_precmd'

# Prepend PROMPT marker to PS1
# This marks the exact boundary where prompt output ends
PS1="\[\e]777;${{__wrash_token}};PROMPT\a\]${{PS1}}"

# Preexec function emits PREEXEC marker before command execution
__wrash_preexec() {{
    # Skip if this is a completion or internal function
    [[ "$BASH_COMMAND" == "$PROMPT_COMMAND" ]] && return
    [[ "$BASH_COMMAND" == __wrash_* ]] && return
    # Emit PREEXEC marker
    printf '\e]777;%s;PREEXEC\a' "$__wrash_token"
}}

# Install DEBUG trap for preexec functionality
trap '__wrash_preexec' DEBUG
"##,
        token = token_hex
    );

    file.write_all(bashrc_content.as_bytes())
        .context("Failed to write bashrc content")?;

    file.flush().context("Failed to flush bashrc file")?;

    // Keep the file (prevent deletion on drop) and get the path
    let (_, path) = file
        .keep()
        .context("Failed to persist temporary bashrc file")?;

    let path_str = path
        .to_str()
        .context("Bashrc path is not valid UTF-8")?
        .to_string();

    tracing::info!(path = %path_str, "Generated bashrc with session token");

    Ok((path_str, session_token))
}

/// Converts a byte slice to a lowercase hexadecimal string.
///
/// Each byte is converted to two hex characters.
///
/// # Example
///
/// ```ignore
/// let bytes = [0xde, 0xad, 0xbe, 0xef];
/// assert_eq!(bytes_to_hex_string(&bytes), "deadbeef");
/// ```
fn bytes_to_hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_bytes_to_hex_string() {
        assert_eq!(bytes_to_hex_string(&[]), "");
        assert_eq!(bytes_to_hex_string(&[0x00]), "00");
        assert_eq!(bytes_to_hex_string(&[0xff]), "ff");
        assert_eq!(bytes_to_hex_string(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        assert_eq!(
            bytes_to_hex_string(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]),
            "0123456789abcdef"
        );
    }

    #[test]
    fn test_generate_creates_file() {
        let (path, token) = generate().expect("generate() failed");

        // File should exist
        assert!(
            std::path::Path::new(&path).exists(),
            "Generated bashrc should exist at {}",
            path
        );

        // Token should be 16 bytes (hex chars)
        assert_eq!(token.len(), 16);

        // Token should be valid ASCII hex
        for &b in &token {
            assert!(
                b.is_ascii_hexdigit(),
                "Token byte {:02x} is not hex",
                b
            );
        }

        // Clean up
        fs::remove_file(&path).ok();
    }

    #[test]
    fn test_generate_file_contains_token() {
        let (path, token) = generate().expect("generate() failed");

        let content = fs::read_to_string(&path).expect("Failed to read bashrc");
        let token_str = std::str::from_utf8(&token).expect("Token not UTF-8");

        // File should contain the token
        assert!(
            content.contains(token_str),
            "Bashrc should contain session token"
        );

        // File should contain marker emission
        assert!(content.contains("PRECMD"), "Bashrc should emit PRECMD");
        assert!(content.contains("PROMPT"), "Bashrc should emit PROMPT");
        assert!(content.contains("PREEXEC"), "Bashrc should emit PREEXEC");

        // File should source user's bashrc
        assert!(
            content.contains("source ~/.bashrc"),
            "Bashrc should source user's bashrc"
        );

        // Clean up
        fs::remove_file(&path).ok();
    }

    #[test]
    fn test_generate_unique_tokens() {
        let (path1, token1) = generate().expect("generate() 1 failed");
        let (path2, token2) = generate().expect("generate() 2 failed");

        // Tokens should be different
        assert_ne!(token1, token2, "Tokens should be unique");

        // Paths should be different
        assert_ne!(path1, path2, "Paths should be unique");

        // Clean up
        fs::remove_file(&path1).ok();
        fs::remove_file(&path2).ok();
    }
}
