//! Processing: what happens to a file after its import. The processing services (a worker
//! that rewrites files, a bridge that hands files to it and has the results imported) keep
//! their own history. This module reads it, keeps every run it has seen, links each run to the
//! title whose folder its files lie in, and says which runs are open, which a worker is on,
//! and which are stuck.
//!
//! The pass and the pages only read the services: they ask them through the `Processor` port
//! (`engines`), with GET and with the reads a service takes in a POST (a worker's own
//! records: its libraries, its flows, its totals, a file by its path). One thing is done to a
//! service, and only when the owner asks for it and the integration's access is `act`: a file
//! is handed to one of its flows (`service::send`). The words for a service's flows and
//! triggers are its adapter's, and so is the tag that keeps a title away from it; this module
//! knows the states of a run and nothing of any one service.

pub mod model;
pub mod rules;
pub mod service;
pub mod store;
