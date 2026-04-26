#![cfg_attr(test, allow(clippy::clone_on_copy))]

include!("tests/support_failover.rs");
include!("tests/commands_accounts_01.rs");
include!("tests/commands_accounts_02.rs");
include!("tests/state_model_poke_01.rs");
include!("tests/state_model_poke_02.rs");
include!("tests/state_model_poke_03.rs");
include!("tests/remote_startup_input_01.rs");
include!("tests/remote_startup_input_02.rs");
include!("tests/remote_startup_input_03.rs");
include!("tests/remote_startup_input_04.rs");
include!("tests/remote_events_reload_01.rs");
include!("tests/remote_events_reload_02.rs");
include!("tests/remote_events_reload_03.rs");
include!("tests/remote_events_reload_04.rs");
include!("tests/scroll_copy_01.rs");
include!("tests/scroll_copy_02.rs");
include!("tests/scroll_copy_03.rs");
