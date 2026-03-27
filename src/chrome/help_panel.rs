//! Help section type shared between help views.

/// A section of help content.
#[derive(Debug, Clone)]
pub struct HelpSection {
    /// Section title.
    pub title: String,
    /// Key-description pairs.
    pub entries: Vec<(String, String)>,
}
