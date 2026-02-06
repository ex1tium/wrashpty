//! Feature-specific state and logic for the scroll viewer.
//!
//! Each feature (search, filter, yank, etc.) has its own submodule
//! containing its state struct and associated logic.

mod filter;
mod goto;
mod help;
mod search;
mod yank;

pub use filter::FilterState;
pub use goto::GoToLineState;
pub use help::HelpBar;
pub use search::{SearchDirection, SearchMatch, SearchState};
pub use yank::{SelectionMode, YankState};
