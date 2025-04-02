use clap::Parser;
use std::str::FromStr;

mod openevse;

/// Read energy consumption & generation information from Enphase Envoy,
/// allow any surplus to be used by OpenEVSE to charge an EV.
#[derive(clap::Parser, Debug)]
#[command(version, about, long_about=None)]
struct Args {
    /// The hostname or IP address of the Enphase Envoy to connect to.
    #[arg(long, default_value_t = String::from("envoy.local"))]
    envoy: String,

    /// The hostname or IP address of the OpenEVSE to connect to.
    #[arg(long, default_value_t = String::from("openevse"))]
    openevse: String,

    /// The MQTT broker to connect to for OpenEVSE telemetry.
    #[arg(long)]
    mqtt_broker: String,

    /// The Envoy local auth token to use, uuencoded.
    #[arg(short, long)]
    auth_token: String,

    /// The number of seconds between updates.
    #[arg(short, long, default_value_t = 60)]
    period: u64,

    /// The target amount of current to be exporting.  Anything above
    /// this surplus will be directed to the EVSE.
    #[arg(short = 't', long, default_value_t = 1.0)]
    target_export_current: f64,

    /// Minimum EVSE charge current.  If there's less than this available,
    /// the EVSE will be put to sleep, where it won't charge the EV.
    #[arg(short = 'i', long, default_value_t = 6.0)]
    evse_min_charge_current: f64,

    /// Maximum EVSE charge current.  If there's more than this available,
    /// the surplus will be exported instead of used by the EVSE.
    #[arg(short = 'x', long, default_value_t = 30.0)]
    evse_max_charge_current: f64,
}

#[derive(Debug)]
struct State {
    envoy: enphase_local::Envoy,
    openevse: openevse::OpenEVSE,

    // "Enphase Integrated Meter", measures energy produced and consumed.
    net_eim: Option<enphase_local::production::Device>,

    // How many Amps we're currently exporting to the grid.
    export_current: f64,

    // The EVSE Pilot current, how much it's advertising to the EV that
    // it's willing to supply.
    evse_charge_limit: f64,

    // The EVSE actual charge current.  How much the EV is currently
    // drawing.
    evse_charge_current: f64,
}

impl State {
    async fn get_net_eim(&self) -> Result<enphase_local::production::Device, eyre::Report> {
        let production = self.envoy.production().await?;
        production
            .consumption
            .into_iter()
            .find(|device| {
                device.type_ == enphase_local::production::DeviceType::Eim
                    && device.measurement_type.unwrap()
                        == enphase_local::production::MeasurementType::NetConsumption
            })
            .ok_or(eyre::eyre!("no net integrated meter found"))
    }

    async fn update_current_surplus(&mut self) -> Result<(), eyre::Report> {
        let net_eim = self.get_net_eim().await?;
        let details = net_eim.details.as_ref().unwrap();

        match &self.net_eim {
            None => {
                println!(
                    "no previous reading to compare to, using instantaneous data for this cycle"
                );
                self.export_current = -net_eim.w_now / details.rms_voltage;
            }
            Some(old_net_eim) => {
                let time_delta = net_eim.reading_time - old_net_eim.reading_time;

                // Enphase reports second-resolution timestamps, it'd
                // be nice if it had higher resolution.
                let time_delta_s = time_delta.num_seconds() as f64;

                let wh = details.wh_lifetime - old_net_eim.details.as_ref().unwrap().wh_lifetime;
                let ws = wh * 60.0 * 60.0;
                let w = ws / time_delta_s;

                // Average current consumed from the grid during the
                // time interval from the old reading to now.  If this is
                // positive, it means we imported energy from the grid.
                // If it's negative we exported to the grid.
                let a = w / details.rms_voltage;

                self.export_current = -a;
            }
        }
        self.net_eim = Some(net_eim);
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), eyre::Report> {
    let args = Args::parse();
    println!("config: {args:#?}");

    let envoy = enphase_local::Envoy::new(
        reqwest::Url::parse(&format!("https://{}", &args.envoy))?,
        &args.auth_token,
    );

    let openevse = openevse::OpenEVSE::new(&args.openevse);
    let active_charging_current = openevse.get_active_charging_current().await?;
    // FIXME: only if the charger's enabled, not sleeping
    let charging_current_limit = openevse.get_current_capacity().await?;

    let mqtt_options = rumqttc::MqttOptions::new("rumqttc-async", args.mqtt_broker, 1883);

    let (mqtt_client, mut mqtt_eventloop) = rumqttc::AsyncClient::new(mqtt_options, 10);
    mqtt_client
        .subscribe("openevse/amp", rumqttc::QoS::AtMostOnce)
        .await
        .unwrap();
    mqtt_client
        .subscribe("openevse/pilot", rumqttc::QoS::AtMostOnce)
        .await
        .unwrap();

    let mut state = State {
        envoy: envoy,
        openevse: openevse,
        net_eim: None,
        export_current: 0.0,
        evse_charge_current: active_charging_current,
        evse_charge_limit: charging_current_limit,
    };

    // My OpenEVSE has a minimum charge current of 6A (1.5 kW).
    // We should probably avoid clicking the relay on/off too much.
    loop {
        let now = chrono::Local::now();
        println!("{}", now.format("%Y-%m-%d %H:%M:%S"));

        state.update_current_surplus().await?;
        println!(
            "export current: {:.3} A (target {:.3} A)",
            state.export_current, args.target_export_current
        );

        println!("old evse charge limit: {:.3} A", state.evse_charge_limit);
        state.evse_charge_limit = (state.evse_charge_limit + state.export_current
            - args.target_export_current)
            .clamp(0.0, args.evse_max_charge_current);
        if state.evse_charge_limit < args.evse_min_charge_current {
            state.evse_charge_limit = 0.0;
        }
        println!("new evse charge limit: {:.3} A", state.evse_charge_limit);

        if state.evse_charge_limit >= args.evse_min_charge_current {
            // There's enough available power to charge the car.
            println!("charging at {:.3} A!", state.evse_charge_limit);

            // Update the OpenEVSE with the new charge limit.
            state
                .openevse
                .set_current_capacity(state.evse_charge_limit as isize)
                .await?;
            state.openevse.get_current_capacity().await?;

            state.openevse.enable().await?;
        } else {
            println!("sleeping, waiting for more available current");
            state.openevse.sleep().await?;
        }

        let timeout = tokio::time::sleep(tokio::time::Duration::from_secs(args.period));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                notification = mqtt_eventloop.poll() => {
                    match notification {
                        Ok(rumqttc::Event::Incoming(rumqttc::mqttbytes::v4::Packet::Publish(msg))) => {
                            let payload = String::from_utf8_lossy(&msg.payload);
                            match msg.topic.as_str() {
                                "openevse/amp" => {
                                    match f64::from_str(&payload) {
                                        Ok(new_val) => {
                                            state.evse_charge_current = new_val / 1000.0;
                                            println!("EVSE reports active charge current: {:.3}", state.evse_charge_current);
                                        }
                                        Err(e) => {
                                            println!("failed to parse f64 from {:#?}: {:#?}", payload, e);
                                            state.evse_charge_current = 0.0;
                                        }
                                    }
                                }
                                "openevse/pilot" => {
                                    match f64::from_str(&payload) {
                                        Ok(new_val) => {
                                            println!("EVSE reports charge current limit: {:.3}", new_val);
                                        }
                                        Err(e) => {
                                            println!("failed to parse f64 from {:#?}: {:#?}", payload, e);
                                        }
                                    }
                                }
                                _ => {
                                    ()
                                }
                            }
                        }
                        _ => {
                            ()
                        }
                    }
                }

                _ = &mut timeout => {
                    break;
                }
            }
        }

        println!("");
    }
}
