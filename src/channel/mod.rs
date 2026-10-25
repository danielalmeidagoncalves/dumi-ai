//! Channels are the ways dumi talks to humans: the terminal today, Jami next.
//!
//! Every channel does the same two things: hand user messages to the agent,
//! and deliver the agent's replies (plus fired reminders) back to the user.

pub mod console;
