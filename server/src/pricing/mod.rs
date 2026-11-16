//! Price and catalog synchronization.
//!
//! Only OpenRouter has a built-in sync (decision D10); every other provider's
//! catalog and prices are entered manually. An unknown price is stored as
//! NULL and scored as unknown — never as zero.

pub mod openrouter;
