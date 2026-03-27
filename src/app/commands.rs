//! Built-in colon command system.
//!
//! Commands are prefixed with `:` and intercepted before being sent to the
//! shell. The command registry holds all registered commands and handles
//! dispatch, including two-step confirmation flows for destructive operations.
//!
//! Commands work from the reedline prompt (Edit mode). The colon prefix was
//! chosen for vim familiarity and because `:` is a bash no-op — if a command
//! isn't recognized, it can be forwarded to the shell harmlessly.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing::warn;

use crate::chrome::glyphs::GlyphTier;
use crate::chrome::{Chrome, NotificationStyle};
use crate::history_store::HistoryStore;

/// What the app should do after a command executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandAction {
    /// Command handled, stay in Edit mode.
    Handled,
    /// Open the panel browser.
    OpenPanel,
    /// Open the Settings panel on the Help subtab.
    OpenSettingsHelp,
}

/// Whether a command needs confirmation before executing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirmation {
    /// Execute immediately.
    None,
    /// Require the user to type a confirmation word.
    Required(&'static str),
}

/// A registered command definition.
struct CommandDef {
    /// Primary name (without the `:` prefix).
    name: &'static str,
    /// Optional aliases (without the `:` prefix).
    aliases: &'static [&'static str],
    /// Short description for help output.
    description: &'static str,
    /// Whether this command requires confirmation.
    confirmation: Confirmation,
    /// The handler function. Takes mutable refs to chrome + history_store.
    handler: fn(&mut CommandContext) -> CommandAction,
}

impl fmt::Debug for CommandDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandDef")
            .field("name", &self.name)
            .field("aliases", &self.aliases)
            .field("description", &self.description)
            .field("confirmation", &self.confirmation)
            .finish()
    }
}

/// Mutable context passed to command handlers.
pub struct CommandContext<'a> {
    pub chrome: &'a mut Chrome,
    pub history_store: &'a Arc<Mutex<HistoryStore>>,
    /// The full argument string after the command name (empty if no args).
    pub args: &'a str,
}

/// Registry of built-in colon commands.
pub struct CommandRegistry {
    /// Commands indexed by primary name.
    commands: Vec<CommandDef>,
    /// Maps names and aliases to command index.
    lookup: HashMap<&'static str, usize>,
    /// Pending confirmation: (command index, confirmation word, expires_at).
    pending_confirmation: Option<(usize, &'static str, Instant)>,
}

const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(10);

impl fmt::Debug for CommandRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.commands.iter().map(|c| c.name).collect();
        f.debug_struct("CommandRegistry")
            .field("commands", &names)
            .finish()
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandRegistry {
    /// Creates a registry with all built-in commands.
    pub fn new() -> Self {
        let mut registry = Self {
            commands: Vec::new(),
            lookup: HashMap::new(),
            pending_confirmation: None,
        };

        registry.register(CommandDef {
            name: "panel",
            aliases: &["p"],
            description: "Open the command browser panel",
            confirmation: Confirmation::None,
            handler: cmd_panel,
        });

        registry.register(CommandDef {
            name: "wipe",
            aliases: &[],
            description: "Delete all command history",
            confirmation: Confirmation::Required("wipe"),
            handler: cmd_wipe,
        });

        registry.register(CommandDef {
            name: "dedupe",
            aliases: &[],
            description: "Remove duplicate history entries",
            confirmation: Confirmation::Required("dedupe"),
            handler: cmd_dedupe,
        });

        registry.register(CommandDef {
            name: "wipe-ci",
            aliases: &[],
            description: "Reset the intelligence database",
            confirmation: Confirmation::Required("wipe"),
            handler: cmd_wipe_ci,
        });

        registry.register(CommandDef {
            name: "glyph-ascii",
            aliases: &[],
            description: "Switch to ASCII glyph tier",
            confirmation: Confirmation::None,
            handler: cmd_glyph_ascii,
        });

        registry.register(CommandDef {
            name: "glyph-unicode",
            aliases: &[],
            description: "Switch to Unicode glyph tier",
            confirmation: Confirmation::None,
            handler: cmd_glyph_unicode,
        });

        registry.register(CommandDef {
            name: "glyph-emoji",
            aliases: &[],
            description: "Switch to Emoji glyph tier",
            confirmation: Confirmation::None,
            handler: cmd_glyph_emoji,
        });

        registry.register(CommandDef {
            name: "glyph-nerdfont",
            aliases: &["glyph-nerd"],
            description: "Switch to NerdFont glyph tier",
            confirmation: Confirmation::None,
            handler: cmd_glyph_nerdfont,
        });

        registry.register(CommandDef {
            name: "help",
            aliases: &["h", "?"],
            description: "List available commands",
            confirmation: Confirmation::None,
            handler: cmd_help,
        });

        registry
    }

    fn register(&mut self, def: CommandDef) {
        let idx = self.commands.len();
        self.lookup.insert(def.name, idx);
        for alias in def.aliases {
            self.lookup.insert(alias, idx);
        }
        self.commands.push(def);
    }

    /// Tries to dispatch a colon command.
    ///
    /// Returns `Some(action)` if the input was a recognized command (or
    /// confirmation), `None` if the input is not a colon command and should
    /// be handled normally (e.g., sent to the shell).
    pub fn dispatch(
        &mut self,
        input: &str,
        chrome: &mut Chrome,
        history_store: &Arc<Mutex<HistoryStore>>,
    ) -> Option<CommandAction> {
        let trimmed = input.trim();

        self.expire_pending_confirmation(chrome);

        // Check for pending confirmation first
        if let Some((cmd_idx, confirm_word, expires_at)) = self.pending_confirmation.take() {
            if Instant::now() <= expires_at && trimmed == confirm_word {
                chrome.clear_notifications();
                let def = &self.commands[cmd_idx];
                let mut ctx = CommandContext {
                    chrome,
                    history_store,
                    args: "",
                };
                return Some((def.handler)(&mut ctx));
            }
            // Wrong confirmation word — clear and fall through
        }

        // Must start with ':'
        if !trimmed.starts_with(':') {
            return None;
        }

        let without_colon = &trimmed[1..];
        if without_colon.is_empty() {
            return None;
        }

        // Split into command name and args
        let (cmd_name, args) = match without_colon.find(char::is_whitespace) {
            Some(pos) => (&without_colon[..pos], without_colon[pos..].trim_start()),
            None => (without_colon, ""),
        };

        let cmd_name_lower = cmd_name.to_lowercase();
        let idx = match self.lookup.get(cmd_name_lower.as_str()) {
            Some(&idx) => idx,
            None => {
                chrome.notify(
                    format!("Unknown command: :{cmd_name}"),
                    NotificationStyle::Error,
                    Duration::from_secs(3),
                );
                return Some(CommandAction::Handled);
            }
        };

        let def = &self.commands[idx];

        // Handle confirmation flow
        if let Confirmation::Required(word) = def.confirmation {
            self.pending_confirmation = Some((idx, word, Instant::now() + CONFIRMATION_TIMEOUT));
            chrome.notify(
                format!("Type '{}' to confirm: {}", word, def.description),
                NotificationStyle::Warning,
                CONFIRMATION_TIMEOUT,
            );
            return Some(CommandAction::Handled);
        }

        // Execute immediately
        let mut ctx = CommandContext {
            chrome,
            history_store,
            args,
        };
        Some((def.handler)(&mut ctx))
    }

    /// Clears any pending confirmation state.
    ///
    /// Called when the user enters a non-confirmation input to cancel
    /// any pending destructive operation.
    pub fn clear_pending(&mut self) {
        self.pending_confirmation = None;
    }

    /// Clears a stale pending confirmation and the matching toast state.
    pub fn expire_pending_confirmation(&mut self, chrome: &mut Chrome) {
        if matches!(
            self.pending_confirmation,
            Some((_, _, expires_at)) if Instant::now() > expires_at
        ) {
            self.pending_confirmation = None;
            chrome.clear_notifications();
        }
    }

    /// Returns whether a confirmation is pending.
    pub fn has_pending_confirmation(&self) -> bool {
        matches!(
            self.pending_confirmation,
            Some((_, _, expires_at)) if Instant::now() <= expires_at
        )
    }

    /// Returns all command definitions for help display.
    pub fn command_list(&self) -> Vec<(&'static str, &'static [&'static str], &'static str)> {
        self.commands
            .iter()
            .map(|def| (def.name, def.aliases, def.description))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Command handlers
// ---------------------------------------------------------------------------

fn cmd_panel(_ctx: &mut CommandContext) -> CommandAction {
    CommandAction::OpenPanel
}

fn notify_history_store_lock_failure(
    chrome: &mut Chrome,
    action: &str,
    error: impl fmt::Display,
) -> CommandAction {
    chrome.notify(
        format!("Failed to {action}: history store lock poisoned ({error})"),
        NotificationStyle::Error,
        Duration::from_secs(5),
    );
    CommandAction::Handled
}

fn cmd_wipe(ctx: &mut CommandContext) -> CommandAction {
    match ctx.history_store.lock() {
        Ok(store) => match store.wipe("wipe") {
            Ok(()) => {
                ctx.chrome.notify(
                    "History database deleted",
                    NotificationStyle::Success,
                    Duration::from_secs(3),
                );
            }
            Err(e) => {
                ctx.chrome.notify(
                    format!("Failed to delete history: {e}"),
                    NotificationStyle::Error,
                    Duration::from_secs(5),
                );
            }
        },
        Err(e) => return notify_history_store_lock_failure(ctx.chrome, "delete history", e),
    }
    CommandAction::Handled
}

fn cmd_dedupe(ctx: &mut CommandContext) -> CommandAction {
    match ctx.history_store.lock() {
        Ok(store) => match store.dedupe_all() {
            Ok((sqlite_removed, bash_removed)) => {
                let msg = format!(
                    "Removed {} duplicates (SQLite: {}, bash_history: {})",
                    sqlite_removed + bash_removed,
                    sqlite_removed,
                    bash_removed
                );
                ctx.chrome
                    .notify(msg, NotificationStyle::Success, Duration::from_secs(5));
            }
            Err(e) => {
                ctx.chrome.notify(
                    format!("Failed to dedupe history: {e}"),
                    NotificationStyle::Error,
                    Duration::from_secs(5),
                );
            }
        },
        Err(e) => return notify_history_store_lock_failure(ctx.chrome, "dedupe history", e),
    }
    CommandAction::Handled
}

fn cmd_wipe_ci(ctx: &mut CommandContext) -> CommandAction {
    match ctx.history_store.lock() {
        Ok(mut store) => match store.reset_intelligence() {
            Ok(()) => {
                ctx.chrome.notify(
                    "Intelligence database reset",
                    NotificationStyle::Success,
                    Duration::from_secs(3),
                );
            }
            Err(e) => {
                ctx.chrome.notify(
                    format!("Failed to reset intelligence: {e}"),
                    NotificationStyle::Error,
                    Duration::from_secs(5),
                );
            }
        },
        Err(e) => {
            return notify_history_store_lock_failure(ctx.chrome, "reset intelligence", e);
        }
    }
    CommandAction::Handled
}

fn set_glyph_tier(ctx: &mut CommandContext, tier: GlyphTier) -> CommandAction {
    ctx.chrome.set_glyph_tier(tier);
    // Persist preference
    match ctx.history_store.lock() {
        Ok(store) => {
            if let Err(e) = store.set_setting("glyph_tier", tier.label()) {
                warn!("Failed to persist glyph tier: {e}");
            }
        }
        Err(e) => {
            return notify_history_store_lock_failure(ctx.chrome, "persist glyph tier setting", e);
        }
    }
    ctx.chrome.notify(
        format!("Glyphs: {}", tier.label()),
        NotificationStyle::Info,
        Duration::from_secs(2),
    );
    CommandAction::Handled
}

fn cmd_glyph_ascii(ctx: &mut CommandContext) -> CommandAction {
    set_glyph_tier(ctx, GlyphTier::Ascii)
}

fn cmd_glyph_unicode(ctx: &mut CommandContext) -> CommandAction {
    set_glyph_tier(ctx, GlyphTier::Unicode)
}

fn cmd_glyph_emoji(ctx: &mut CommandContext) -> CommandAction {
    set_glyph_tier(ctx, GlyphTier::Emoji)
}

fn cmd_glyph_nerdfont(ctx: &mut CommandContext) -> CommandAction {
    set_glyph_tier(ctx, GlyphTier::NerdFont)
}

fn cmd_help(ctx: &mut CommandContext) -> CommandAction {
    if ctx.args.is_empty() {
        // Open settings panel on help subtab
        CommandAction::OpenSettingsHelp
    } else {
        // Show condensed help as notification with search hint
        let help = "\
:panel (:p)        Open command browser\n\
:glyph-ascii       Switch to ASCII glyphs\n\
:glyph-unicode     Switch to Unicode glyphs\n\
:glyph-emoji       Switch to Emoji glyphs\n\
:glyph-nerdfont    Switch to NerdFont glyphs\n\
:wipe              Delete all history\n\
:dedupe            Remove duplicate history\n\
:wipe-ci           Reset intelligence DB\n\
:help (:h, :?)     Open help panel";

        ctx.chrome
            .notify(help, NotificationStyle::Info, Duration::from_secs(15));
        CommandAction::Handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_chrome() -> Chrome {
        // Chrome::new requires a Config — use a minimal approach
        // We test dispatch logic via the registry directly
        Chrome::new(
            crate::types::ChromeMode::Full,
            &crate::config::Config::default(),
        )
    }

    fn make_test_store() -> Arc<Mutex<HistoryStore>> {
        // Use a unique temp file so parallel tests don't share the real history.db
        Arc::new(Mutex::new(
            HistoryStore::new_temp().expect("test history store"),
        ))
    }

    #[test]
    fn test_registry_has_expected_commands() {
        let registry = CommandRegistry::new();
        assert!(registry.lookup.contains_key("panel"));
        assert!(registry.lookup.contains_key("p"));
        assert!(registry.lookup.contains_key("wipe"));
        assert!(registry.lookup.contains_key("dedupe"));
        assert!(registry.lookup.contains_key("wipe-ci"));
        assert!(registry.lookup.contains_key("glyph-ascii"));
        assert!(registry.lookup.contains_key("glyph-unicode"));
        assert!(registry.lookup.contains_key("glyph-emoji"));
        assert!(registry.lookup.contains_key("glyph-nerdfont"));
        assert!(registry.lookup.contains_key("glyph-nerd"));
        assert!(registry.lookup.contains_key("help"));
        assert!(registry.lookup.contains_key("h"));
        assert!(registry.lookup.contains_key("?"));
    }

    #[test]
    fn test_dispatch_non_colon_returns_none() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();
        assert!(registry.dispatch("ls -la", &mut chrome, &store).is_none());
    }

    #[test]
    fn test_dispatch_empty_colon_returns_none() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();
        assert!(registry.dispatch(":", &mut chrome, &store).is_none());
    }

    #[test]
    fn test_dispatch_panel_returns_open_panel() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();
        assert_eq!(
            registry.dispatch(":panel", &mut chrome, &store),
            Some(CommandAction::OpenPanel)
        );
    }

    #[test]
    fn test_dispatch_panel_alias_open_panel() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();
        assert_eq!(
            registry.dispatch(":p", &mut chrome, &store),
            Some(CommandAction::OpenPanel)
        );
    }

    #[test]
    fn test_dispatch_unknown_command_returns_handled() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();
        assert_eq!(
            registry.dispatch(":nonexistent", &mut chrome, &store),
            Some(CommandAction::Handled)
        );
    }

    #[test]
    fn test_dispatch_glyph_ascii_changes_tier() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();
        let result = registry.dispatch(":glyph-ascii", &mut chrome, &store);
        assert_eq!(result, Some(CommandAction::Handled));
        assert_eq!(chrome.glyph_tier(), GlyphTier::Ascii);
    }

    #[test]
    fn test_dispatch_glyph_nerdfont_alias() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();
        let result = registry.dispatch(":glyph-nerd", &mut chrome, &store);
        assert_eq!(result, Some(CommandAction::Handled));
        assert_eq!(chrome.glyph_tier(), GlyphTier::NerdFont);
    }

    #[test]
    fn test_dispatch_case_insensitive_open_panel() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();
        assert_eq!(
            registry.dispatch(":PANEL", &mut chrome, &store),
            Some(CommandAction::OpenPanel)
        );
    }

    #[test]
    fn test_dispatch_wipe_confirmation_executes_handled() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();

        // First dispatch sets pending confirmation
        let result = registry.dispatch(":wipe", &mut chrome, &store);
        assert_eq!(result, Some(CommandAction::Handled));
        assert!(registry.has_pending_confirmation());

        // Typing the confirmation word executes
        let result = registry.dispatch("wipe", &mut chrome, &store);
        assert_eq!(result, Some(CommandAction::Handled));
        assert!(!registry.has_pending_confirmation());
    }

    #[test]
    fn test_confirmation_cancelled_by_other_input() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();

        // Set pending
        registry.dispatch(":wipe", &mut chrome, &store);
        assert!(registry.has_pending_confirmation());

        // Wrong word clears pending (returns None because it's not a colon command,
        // but the pending confirmation is consumed by the attempt)
        let _ = registry.dispatch("nope", &mut chrome, &store);
        assert!(!registry.has_pending_confirmation());
    }

    #[test]
    fn test_dispatch_when_confirmation_expired_clears_pending() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();

        registry.pending_confirmation = Some((1, "wipe", Instant::now() - Duration::from_secs(1)));

        assert_eq!(registry.dispatch("wipe", &mut chrome, &store), None);
        assert!(!registry.has_pending_confirmation());
    }

    #[test]
    fn test_dispatch_glyph_emoji_persists_setting() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();

        registry.dispatch(":glyph-emoji", &mut chrome, &store);

        let saved = store.lock().unwrap().get_setting("glyph_tier").unwrap();
        assert_eq!(saved, Some("Emoji".to_string()));
    }

    #[test]
    fn test_command_list_contains_expected_entries() {
        let registry = CommandRegistry::new();
        let list = registry.command_list();
        assert!(list.iter().any(|(name, _, _)| *name == "panel"));
        assert!(list.iter().any(|(name, _, _)| *name == "glyph-ascii"));
        assert!(list.iter().any(|(name, _, _)| *name == "help"));
    }

    #[test]
    fn test_dispatch_with_whitespace() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();
        assert_eq!(
            registry.dispatch("  :panel  ", &mut chrome, &store),
            Some(CommandAction::OpenPanel)
        );
    }

    #[test]
    fn test_help_no_args_opens_settings_help() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();
        assert_eq!(
            registry.dispatch(":help", &mut chrome, &store),
            Some(CommandAction::OpenSettingsHelp)
        );
    }

    #[test]
    fn test_help_with_args_returns_handled() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();
        assert_eq!(
            registry.dispatch(":help panel", &mut chrome, &store),
            Some(CommandAction::Handled)
        );
    }

    #[test]
    fn test_dispatch_help_alias_question_mark_opens_settings_help() {
        let mut registry = CommandRegistry::new();
        let mut chrome = make_test_chrome();
        let store = make_test_store();
        assert_eq!(
            registry.dispatch(":?", &mut chrome, &store),
            Some(CommandAction::OpenSettingsHelp)
        );
    }
}
