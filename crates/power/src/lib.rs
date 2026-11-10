use anyhow::Result;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{AnyOutputPin, Output, PinDriver};

pub trait PowerControl {
    fn power_on(&mut self) -> Result<()>;
    fn power_off(&mut self) -> Result<()>;
    fn is_on(&self) -> Result<bool>;
}

pub struct SimcomPower<'d> {
    pwrkey: PinDriver<'d, Output>,
    power_en: Option<PinDriver<'d, Output>>,
}

impl<'d> SimcomPower<'d> {
    pub fn new(
        pwrkey_pin: AnyOutputPin<'d>,
        power_en_pin: Option<AnyOutputPin<'d>>,
    ) -> Result<Self> {
        let pwrkey = PinDriver::output(pwrkey_pin)?;
        let power_en = match power_en_pin {
            Some(pin) => Some(PinDriver::output(pin)?),
            None => None,
        };
        Ok(Self { pwrkey, power_en })
    }

    pub fn lilygo(
        poweron_pin: AnyOutputPin<'d>,
        _reset_pin: AnyOutputPin<'d>,
        pwrkey_pin: AnyOutputPin<'d>,
    ) -> Result<Self> {
        Self::new(pwrkey_pin, Some(poweron_pin))
    }
}

impl<'d> PowerControl for SimcomPower<'d> {
    fn power_on(&mut self) -> Result<()> {
        if let Some(ref mut en) = self.power_en {
            en.set_high()?;
            FreeRtos::delay_ms(100);
        }

        self.pwrkey.set_high()?;
        FreeRtos::delay_ms(50);
        self.pwrkey.set_low()?;
        FreeRtos::delay_ms(600);
        self.pwrkey.set_high()?;
        FreeRtos::delay_ms(2000);

        log::info!("Modem power-on sequence complete");
        Ok(())
    }

    fn power_off(&mut self) -> Result<()> {
        log::info!("Powering off modem");
        self.pwrkey.set_low()?;
        FreeRtos::delay_ms(600);
        self.pwrkey.set_high()?;
        FreeRtos::delay_ms(1000);
        if let Some(ref mut en) = self.power_en {
            en.set_low()?;
        }
        log::info!("Modem powered off");
        Ok(())
    }

    fn is_on(&self) -> Result<bool> {
        Ok(true)
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
