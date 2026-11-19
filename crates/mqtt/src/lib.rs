use std::time::Duration;

use anyhow::Result;
use embedded_svc::mqtt::client::EventPayload;
use esp_idf_svc::mqtt::client::{EspMqttClient, MqttClientConfiguration, QoS};

pub struct MqttConfig<'a> {
    pub broker_url: &'a str,
    pub client_id: &'a str,
    pub username: Option<&'a str>,
    pub password: Option<&'a str>,
}

pub struct MqttClient {
    client: EspMqttClient<'static>,
}

impl MqttClient {
    pub fn new(config: MqttConfig<'_>) -> Result<Self> {
        anyhow::ensure!(
            !config.broker_url.is_empty(),
            "MQTT broker URL cannot be empty"
        );
        anyhow::ensure!(
            !config.client_id.is_empty(),
            "MQTT client ID cannot be empty"
        );

        let client = EspMqttClient::new_cb(
            config.broker_url,
            &MqttClientConfiguration {
                client_id: Some(config.client_id),
                username: config.username,
                password: config.password,
                keep_alive_interval: Some(Duration::from_secs(30)),
                reconnect_timeout: Some(Duration::from_secs(5)),
                network_timeout: Duration::from_secs(10),
                outbox_limit: Some(64),
                ..Default::default()
            },
            move |event| match event.payload() {
                EventPayload::Connected(_) => {
                    log::info!(target: "mqtt", "MQTT connected");
                }
                EventPayload::Disconnected => {
                    log::warn!(target: "mqtt", "MQTT disconnected; reconnecting");
                }
                EventPayload::Error(error) => {
                    log::warn!(target: "mqtt", "MQTT error: {}", error);
                }
                _ => {}
            },
        )?;
        Ok(Self { client })
    }

    pub fn publish(&mut self, topic: &str, payload: &[u8]) -> Result<()> {
        anyhow::ensure!(!topic.is_empty(), "MQTT topic cannot be empty");
        self.client
            .enqueue(topic, QoS::AtLeastOnce, false, payload)?;
        Ok(())
    }
}
