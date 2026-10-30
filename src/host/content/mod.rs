//! Content streams read from a `Read`, one operator at a time, with memory
//! bounded by the largest token and not by the stream.

pub mod lexer;
pub mod operators;

pub use lexer::Lexer;
pub use operators::{Mode, Operators};
