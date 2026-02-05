//! Feature-specific state and logic for the scroll viewer.
//!
//! Each feature (search, filter, yank, etc.) has its own submodule
//! containing its state struct and associated logic.

mod filter;
mod goto;
mod search;
mod yank;

pub use filter::FilterState;
pub use goto::GoToLineState;
pub use search::{SearchDirection, SearchState};
pub use yank::{SelectionMode, YankState};
