//! Version extraction from help and banner output.

use regex::Regex;

/// Extracts a version string from help or banner text.
///
/// Looks for semver-like patterns (`1.2.3`, `v1.2.3-rc1`, etc.) near
/// version keywords or the command name. Returns `None` when no
/// confident match is found.
pub fn extract_version(text: &str, command_name: &str) -> Option<String> {
    let candidates = collect_version_candidates(text, command_name);
    candidates
        .into_iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .filter(|(_, confidence)| *confidence >= 0.4)
        .map(|(version, _)| normalize_version(&version))
}

fn collect_version_candidates(text: &str, command_name: &str) -> Vec<(String, f64)> {
    let version_re = Regex::new(
        r"(?x)
        \b
        v?                              # optional 'v' prefix
        (\d{1,4}\.\d{1,4}(?:\.\d{1,6})?) # major.minor[.patch]
        ([-+][a-zA-Z0-9._+-]*)?        # optional pre-release / build
        \b
        "
    )
    .expect("version regex");

    let mut candidates = Vec::new();
    let cmd_lower = command_name
        .split_whitespace()
        .next()
        .unwrap_or(command_name)
        .to_ascii_lowercase();

    for line in text.lines().take(10) {
        let line_lower = line.to_ascii_lowercase();

        for cap in version_re.captures_iter(line) {
            let full_match = cap.get(0).unwrap();
            let raw = full_match.as_str().to_string();
            let core = cap.get(1).unwrap().as_str();

            // Reject date-like patterns (e.g. 2024.01.15, 2024-01-15)
            if is_likely_date(core) {
                continue;
            }

            // Reject IP-address-like patterns
            if is_likely_ip(core) {
                continue;
            }

            // Reject file-path-like patterns (preceded by / or \)
            let byte_start = full_match.start();
            if byte_start > 0 {
                let prev_char = line.as_bytes()[byte_start - 1];
                if prev_char == b'/' || prev_char == b'\\' {
                    continue;
                }
            }

            // Reject partial IP/dotted-number matches where more components follow
            let byte_end = full_match.end();
            if byte_end < line.len() && line.as_bytes()[byte_end] == b'.' {
                continue;
            }

            let mut confidence: f64 = 0.3;

            // Boost: near "version" keyword
            if line_lower.contains("version") || line_lower.contains("ver ") {
                confidence += 0.4;
            }

            // Boost: command name appears on same line
            if line_lower.contains(&cmd_lower) {
                confidence += 0.2;
            }

            // Boost: has 'v' prefix
            if raw.starts_with('v') || raw.starts_with('V') {
                confidence += 0.1;
            }

            // Boost: appears in first 3 lines (banner)
            let line_index = text.lines().position(|l| std::ptr::eq(l, line));
            if line_index.is_some_and(|i| i < 3) {
                confidence += 0.1;
            }

            // Boost: has 3 components (major.minor.patch)
            if core.matches('.').count() >= 2 {
                confidence += 0.1;
            }

            candidates.push((raw, confidence.min(1.0)));
        }
    }

    candidates
}

fn normalize_version(raw: &str) -> String {
    let trimmed = raw.trim();
    // Strip leading 'v' or 'V' for consistent output
    if trimmed.starts_with('v') || trimmed.starts_with('V') {
        trimmed[1..].to_string()
    } else {
        trimmed.to_string()
    }
}

fn is_likely_date(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() >= 2 {
        if let Ok(first) = parts[0].parse::<u32>() {
            // Year-like first component (2000-2099)
            if (2000..2100).contains(&first) {
                if let Ok(second) = parts[1].parse::<u32>() {
                    // Month-like second component (1-12)
                    if (1..=12).contains(&second) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn is_likely_ip(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() >= 3 {
        // IP addresses have 4 octets, version typically has 2-3 components
        // Check if all components look like octets (0-255)
        let all_octets = parts.iter().all(|p| {
            p.parse::<u32>().is_ok_and(|n| n <= 255)
        });
        // Only reject if it looks like 4 octets (x.x.x.x pattern)
        if parts.len() >= 4 && all_octets {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_banner_style_version() {
        let text = "git version 2.39.1\n";
        assert_eq!(extract_version(text, "git"), Some("2.39.1".to_string()));
    }

    #[test]
    fn test_keyword_adjacent_version() {
        let text = "Version: 1.0.0\nUsage: mycmd [options]";
        assert_eq!(extract_version(text, "mycmd"), Some("1.0.0".to_string()));
    }

    #[test]
    fn test_v_prefix_version() {
        let text = "mycmd v1.2.3-rc1\nUsage: mycmd [options]";
        assert_eq!(extract_version(text, "mycmd"), Some("1.2.3-rc1".to_string()));
    }

    #[test]
    fn test_version_with_build_suffix() {
        let text = "tool version 3.4.5+build123";
        assert_eq!(extract_version(text, "tool"), Some("3.4.5+build123".to_string()));
    }

    #[test]
    fn test_reject_date_pattern() {
        let text = "Released 2024.01.15\nUsage: tool [options]\nFlags:\n  --help";
        // Should not extract 2024.01.15 as a version
        let result = extract_version(text, "tool");
        assert!(result.is_none() || !result.unwrap().starts_with("2024"));
    }

    #[test]
    fn test_reject_ip_address() {
        let text = "Connecting to 192.168.1.1\nUsage: tool [options]";
        let result = extract_version(text, "tool");
        assert!(result.is_none() || !result.unwrap().starts_with("192"));
    }

    #[test]
    fn test_no_version_in_plain_help() {
        let text = "Usage: mycmd [options]\n\nOptions:\n  --help  Show help";
        assert_eq!(extract_version(text, "mycmd"), None);
    }

    #[test]
    fn test_version_two_component() {
        let text = "docker version 24.0\nUsage: docker [OPTIONS] COMMAND";
        assert_eq!(extract_version(text, "docker"), Some("24.0".to_string()));
    }

    #[test]
    fn test_version_alpha_suffix() {
        let text = "kubectl v1.28.0-alpha.1\nUsage: kubectl [flags]";
        assert_eq!(
            extract_version(text, "kubectl"),
            Some("1.28.0-alpha.1".to_string())
        );
    }
}
