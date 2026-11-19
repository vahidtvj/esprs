use anyhow::{anyhow, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};

pub struct WifiConfig<'a> {
    pub ssid: &'a str,
    pub password: &'a str,
}

pub struct Wifi {
    wifi: BlockingWifi<EspWifi<'static>>,
}

impl Wifi {
    pub fn new(
        modem: Modem<'static>,
        sysloop: EspSystemEventLoop,
        nvs: Option<EspDefaultNvsPartition>,
        config: WifiConfig<'_>,
    ) -> Result<Self> {
        anyhow::ensure!(!config.ssid.is_empty(), "WiFi SSID cannot be empty");
        let mut wifi = BlockingWifi::wrap(EspWifi::new(modem, sysloop.clone(), nvs)?, sysloop)?;
        wifi.set_configuration(&Configuration::Client(ClientConfiguration {
            ssid: config
                .ssid
                .try_into()
                .map_err(|_| anyhow!("WiFi SSID exceeds 32 bytes"))?,
            password: config
                .password
                .try_into()
                .map_err(|_| anyhow!("WiFi password exceeds 64 bytes"))?,
            auth_method: if config.password.is_empty() {
                AuthMethod::None
            } else {
                AuthMethod::WPA2Personal
            },
            ..Default::default()
        }))?;
        wifi.start()?;
        Ok(Self { wifi })
    }

    pub fn connect(
        modem: Modem<'static>,
        sysloop: EspSystemEventLoop,
        nvs: Option<EspDefaultNvsPartition>,
        config: WifiConfig<'_>,
    ) -> Result<Self> {
        let mut wifi = Self::new(modem, sysloop, nvs, config)?;
        wifi.ensure_connected()?;
        Ok(wifi)
    }

    pub fn ensure_connected(&mut self) -> Result<()> {
        if self.wifi.is_connected()? {
            return Ok(());
        }

        log::info!(target: "wifi", "Connecting WiFi");
        self.wifi.connect()?;
        self.wifi.wait_netif_up()?;
        log::info!(
            target: "wifi",
            "WiFi connected: {}",
            self.wifi.wifi().sta_netif().get_ip_info()?.ip
        );
        Ok(())
    }
}
