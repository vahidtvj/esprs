use anyhow::Result;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{AnyOutputPin, Output, PinDriver};

pub const MODEM_POWER_ON_DELAY_MS: u32 = 100;
pub const MODEM_PWRKEY_PULSE_MS: u32 = 100;
pub const MODEM_POWEROFF_PULSE_MS: u32 = 3000;
pub const MODEM_BOOT_DELAY_MS: u32 = 3000;
pub const MODEM_RESET_PULSE_MS: u32 = 100;

pub trait PowerControl {
    fn power_on(&mut self) -> Result<()>;
    fn power_off(&mut self) -> Result<()>;
    fn is_on(&self) -> Result<bool>;
}

pub struct SimcomPower<'d> {
    pwrkey: PinDriver<'d, Output>,
    power_en: Option<PinDriver<'d, Output>>,
    reset: Option<PinDriver<'d, Output>>,
    is_on: bool,
}

impl<'d> SimcomPower<'d> {
    pub fn new(
        pwrkey_pin: AnyOutputPin<'d>,
        power_en_pin: Option<AnyOutputPin<'d>>,
    ) -> Result<Self> {
        let mut pwrkey = PinDriver::output(pwrkey_pin)?;
        pwrkey.set_low()?;
        let power_en = match power_en_pin {
            Some(pin) => Some(PinDriver::output(pin)?),
            None => None,
        };
        Ok(Self {
            pwrkey,
            power_en,
            reset: None,
            is_on: false,
        })
    }

    pub fn lilygo(
        poweron_pin: AnyOutputPin<'d>,
        reset_pin: AnyOutputPin<'d>,
        pwrkey_pin: AnyOutputPin<'d>,
    ) -> Result<Self> {
        let mut reset = PinDriver::output(reset_pin)?;
        reset.set_high()?;

        let mut power = Self::new(pwrkey_pin, Some(poweron_pin))?;
        power.reset = Some(reset);
        Ok(power)
    }

    pub fn reset(&mut self) -> Result<()> {
        let reset = self
            .reset
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("modem reset pin is not configured"))?;

        log::info!("Performing modem hard reset");
        reset.set_low()?;
        FreeRtos::delay_ms(MODEM_RESET_PULSE_MS);
        reset.set_high()?;
        FreeRtos::delay_ms(MODEM_BOOT_DELAY_MS);
        log::info!("Modem hard reset complete");
        Ok(())
    }
}

impl<'d> PowerControl for SimcomPower<'d> {
    fn power_on(&mut self) -> Result<()> {
        if self.is_on {
            return Ok(());
        }

        if let Some(ref mut en) = self.power_en {
            en.set_high()?;
            FreeRtos::delay_ms(MODEM_POWER_ON_DELAY_MS);
        }

        self.pwrkey.set_low()?;
        FreeRtos::delay_ms(MODEM_POWER_ON_DELAY_MS);
        self.pwrkey.set_high()?;
        FreeRtos::delay_ms(MODEM_PWRKEY_PULSE_MS);
        self.pwrkey.set_low()?;
        FreeRtos::delay_ms(MODEM_BOOT_DELAY_MS);

        self.is_on = true;
        log::info!("Modem power-on sequence complete");
        Ok(())
    }

    fn power_off(&mut self) -> Result<()> {
        log::info!("Powering off modem");
        self.pwrkey.set_high()?;
        FreeRtos::delay_ms(MODEM_POWEROFF_PULSE_MS);
        self.pwrkey.set_low()?;
        FreeRtos::delay_ms(1000);
        if let Some(ref mut en) = self.power_en {
            en.set_low()?;
        }
        self.is_on = false;
        log::info!("Modem powered off");
        Ok(())
    }

    fn is_on(&self) -> Result<bool> {
        Ok(self.is_on)
    }
}

pub struct NoPower;

impl PowerControl for NoPower {
    fn power_on(&mut self) -> Result<()> {
        Ok(())
    }

    fn power_off(&mut self) -> Result<()> {
        Ok(())
    }

    fn is_on(&self) -> Result<bool> {
        Ok(true)
    }
}
