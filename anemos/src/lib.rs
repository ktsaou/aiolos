//! anemos — the level-2 SDK every anemos (module) reuses.
//!
//! It owns ALL the boilerplate so a module carries only its device-specific logic:
//! - the process lifecycle driver (`run`): CLI dispatch (`detect`/`run <id>`/`restore` + optional
//!   extra subcommands), logging init, SIGTERM/SIGINT handlers, the protocol stdio loops, and the
//!   fail-safe restore wiring;
//! - the signal-aware stdin reader (`StdinReader`) and shutdown handlers;
//! - the temperature→duty `Controller` (live curve reload + EMA + deadband, the 35% floor);
//! - the `Anemos` / `Device` traits a module implements.
//!
//! It depends only on the wire-type `protocol` crate and knows nothing about any device technology
//! (NVML/IPMI/hwmon live in their own level-1 crates, brought in per module). A change to the
//! protocol, CLI, signals, logging, or smoothing is made HERE, once, and every anemos inherits it.

mod controller;
mod curve;
mod damper;
mod run;
mod stdio;

pub use controller::{Controller, Duty};
pub use curve::{Curve, CurveCache};
pub use damper::Damper;
pub use run::{run, run_with, Anemos, Device, ExtraCmd, ModuleInfo};
pub use stdio::{install_shutdown_handlers, shutdown_requested, Event, StdinReader};

// Re-export the wire types so a level-3 anemos needs only `anemos` (+ its tech crates) as deps.
pub use protocol::{Applied, Detected, FoundEntry, Inputs, Reading, Request, Status};
