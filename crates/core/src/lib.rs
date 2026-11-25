//! Pure Gentle Shell logic shared by the app: protocol models, parsers and
//! state transitions. No GPUI, no RPC process, no filesystem, no processes,
//! no OS APIs (see docs/adr/0002-nucleo-puro-incremental.md).

pub mod activity;
pub mod alert_timing;
pub mod backend;
pub mod browser;
pub mod canvas;
pub mod commands;
pub mod contrast;
pub mod devices;
pub mod diagnostics;
pub mod docs;
pub mod double_interrupt;
pub mod engram;
pub mod explorer;
pub mod fonts;
pub mod fuzzy;
pub mod gentle_state;
pub mod genui;
pub mod git;
pub mod glass;
pub mod hydration;
pub mod ides;
pub mod input_style;
pub mod interface_size;
pub mod markdown;
pub mod music;
pub mod notifications;
pub mod preview;
pub mod profiles;
pub mod prompt_history;
pub mod screen;
pub mod scroll;
pub mod sessions;
pub mod setup;
pub mod text;
pub mod tools;
pub mod tower;
pub mod transcript;
pub mod typing_kisses;
pub mod usage;
pub mod version;
pub mod web;
pub mod web_browsers;
pub mod workspace_commands;
pub mod workspaces;
