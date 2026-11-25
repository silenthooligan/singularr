//! Notifications: channels (Slack, Discord, email, a webhook, ntfy, Telegram, Pushover,
//! Gotify), the events each subscribes to, and the messages they are sent.
//!
//! - `model`: the types of channel with the fields each asks for, the events with their groups
//!   and the journal kinds behind them, how a channel's settings are checked. Pure.
//! - `message`: what each type is sent for one event. Pure.
//! - `store`: the channels (their secrets in their rows) and the deliveries.
//! - `service`: changes with their journal events, test messages, the dispatcher that follows
//!   the journal from a cursor per channel, and the sender with its three attempts.
//!
//! A channel's HTTP address goes out through `core::outbound` as a target of its own (POST to
//! that address, nothing else on its host); mail goes out through `core::smtp`.

pub mod message;
pub mod model;
pub mod service;
pub mod store;
