#![no_std]
#![feature(impl_trait_in_assoc_type)]
#![recursion_limit = "256"]

extern crate alloc;

#[cfg(feature = "four_motor")]
compile_error!(
    "`four_motor` feature is incompatible with the ST3215 bus servo: GPIO19 \
     is used by both Motor C and UART1 TX. Build with the default `two_motor` \
     feature instead."
);

pub mod ble;
pub mod brushless;
pub mod commands;
pub mod display;
pub mod http_server;
pub mod serial_cmd;
pub mod st3215;
pub mod wifi;
pub mod wifi_config;
