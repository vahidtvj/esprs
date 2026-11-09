use anyhow::Result;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{AnyOutputPin, Output, PinDriver};
use esp_idf_svc::hal::peripherals::Peripherals;

mod pinout;
use pinout::*;

pub struct ModemPowerPins<'d> {
    pub poweron: PinDriver<'d, Output>,
    pub reset: PinDriver<'d, Output>,
    pub pwrkey: PinDriver<'d, Output>,
}

pub fn modem_power_on<'d>(
    poweron_pin: AnyOutputPin<'d>,
    reset_pin: AnyOutputPin<'d>,
    pwrkey_pin: AnyOutputPin<'d>,
) -> Result<ModemPowerPins<'d>> {
    let mut poweron = PinDriver::output(poweron_pin)?;
    poweron.set_high()?;
    FreeRtos::delay_ms(MODEM_POWER_ON_DELAY_MS);

    let mut reset = PinDriver::output(reset_pin)?;
    reset.set_high()?;

    let mut pwrkey = PinDriver::output(pwrkey_pin)?;
    pwrkey.set_high()?;
    FreeRtos::delay_ms(50);

    pwrkey.set_low()?;
    FreeRtos::delay_ms(MODEM_PWRKEY_PULSE_MS);
    pwrkey.set_high()?;
    FreeRtos::delay_ms(MODEM_BOOT_DELAY_MS);

    log::info!("Modem power-on sequence complete");

    Ok(ModemPowerPins {
        poweron,
        reset,
        pwrkey,
    })
}

pub fn modem_hard_reset<'d>(pins: &mut ModemPowerPins<'d>) -> Result<()> {
    log::info!("Performing modem hard reset");
    pins.reset.set_low()?;
    FreeRtos::delay_ms(MODEM_RESET_PULSE_MS);
    pins.reset.set_high()?;
    FreeRtos::delay_ms(MODEM_BOOT_DELAY_MS);
    log::info!("Modem hard reset complete");
    Ok(())
}

pub fn modem_power_off<'d>(pins: &mut ModemPowerPins<'d>) -> Result<()> {
    log::info!("Powering off modem");
    pins.pwrkey.set_low()?;
    FreeRtos::delay_ms(MODEM_PWRKEY_PULSE_MS);
    pins.pwrkey.set_high()?;
    FreeRtos::delay_ms(1000);
    pins.poweron.set_low()?;
    log::info!("Modem powered off");
    Ok(())
}

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("Starting ESP32 + SIMCOM A7670 modem power-on demo");

    let peripherals = Peripherals::take()?;
    let _modem_pins = modem_power_on(
        peripherals.pins.gpio12.into(),
        peripherals.pins.gpio5.into(),
        peripherals.pins.gpio4.into(),
    )?;

    log::info!("Modem is now powered on and ready. Entering main loop.");

    loop {
        FreeRtos::delay_ms(1000);
    }
}
