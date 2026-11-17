#![allow(dead_code)]

/// SIMCOM A7670 modem configuration for LilyGo T-SIM7600G / similar boards

/// UART baudrate for modem communication
pub const MODEM_BAUDRATE: u32 = 115200;

/// Modem UART pins
pub const MODEM_TX_PIN: i32 = 26;
pub const MODEM_RX_PIN: i32 = 27;
pub const MODEM_DTR_PIN: i32 = 25;

/// Modem control pins
pub const BOARD_PWRKEY_PIN: i32 = 4;
pub const MODEM_RESET_PIN: i32 = 5;
pub const BOARD_POWERON_PIN: i32 = 12;
pub const MODEM_RING_PIN: i32 = 33;

/// Board peripheral pins
pub const BOARD_MISO_PIN: i32 = 2;
pub const BOARD_MOSI_PIN: i32 = 15;
pub const BOARD_SCK_PIN: i32 = 14;
pub const BOARD_SD_CS_PIN: i32 = 13;

/// ADC pins (BOARD_ADC_PIN and BOARD_BAT_ADC_PIN share GPIO35)
pub const BOARD_ADC_PIN: i32 = 35;
pub const BOARD_BAT_ADC_PIN: i32 = 35;

/// Timing constants for modem power sequence (milliseconds)
pub const MODEM_POWER_ON_DELAY_MS: u32 = 100;
pub const MODEM_PWRKEY_PULSE_MS: u32 = 100;
pub const MODEM_POWEROFF_PULSE_MS: u32 = 3000;
pub const MODEM_BOOT_DELAY_MS: u32 = 3000;
pub const MODEM_RESET_PULSE_MS: u32 = 100;

/// Reset level for normal operation (HIGH = not in reset, active LOW reset)
pub const MODEM_RESET_NORMAL_LEVEL: bool = true;
