// The "RAPI" protocol is described here:
// <https://github.com/openenergymonitor/open_evse/blob/master/firmware/open_evse/rapi_proc.h>
//
// ```
// $ curl --silent 'http://openevse/r?json=1&rapi=%24GE' | jq .
// {
//   "cmd": "$GE",
//   "ret": "$OK 30 0121^21"
// }
// ```

use std::str::FromStr;

#[derive(Debug, serde::Deserialize, Clone)]
pub struct RapiReply {
    #[allow(dead_code)]
    cmd: String,
    ret: String,
}

#[derive(Debug)]
pub struct OpenEVSE {
    openevse_hostname: String,
}

impl OpenEVSE {
    pub fn new(openevse_hostname: &str) -> Self {
        Self {
            openevse_hostname: String::from(openevse_hostname),
        }
    }

    pub async fn enable(&self) -> Result<(), eyre::Report> {
        let _data = self.request(&["FE"]).await?;
        // println!("enable: {}", data);
        Ok(())
    }

    pub async fn sleep(&self) -> Result<(), eyre::Report> {
        let _data = self.request(&["FS"]).await?;
        // println!("sleep: {}", data);
        Ok(())
    }

    /// Read amount of current currently being drawn by the EV, in amps.
    pub async fn get_active_charging_current(&self) -> Result<f64, eyre::Report> {
        // `reply` will be a string like "$OK 1234 -1^0C", where the
        // 1234 is the current in milliamps.
        let reply = self.request(&["GG"]).await?;

        let mut tokens = reply.split_whitespace();
        match tokens.next() {
            Some("$OK") => {
                let i = f64::from_str(tokens.next().unwrap())? / 1000.0;
                return Ok(i);
            }
            _ => {
                return Err(eyre::Report::msg(format!("{:#?}", reply)));
            }
        }
    }

    pub async fn get_current_capacity(&self) -> Result<f64, eyre::Report> {
        let reply = self.request(&["GE"]).await?;

        let mut tokens = reply.split_whitespace();
        match tokens.next() {
            Some("$OK") => {
                let i = f64::from_str(tokens.next().unwrap())?;
                return Ok(i);
            }
            _ => {
                return Err(eyre::Report::msg(format!("{:#?}", reply)));
            }
        }
    }

    pub async fn set_current_capacity(
        &self,
        charge_current_limit: isize,
    ) -> Result<(), eyre::Report> {
        let _data = self
            .request(&["SC", &format!("{}", charge_current_limit)])
            .await?;
        // println!("set_current_capacity({}): {}", charge_current_limit, data);
        Ok(())
    }

    pub async fn request(&self, command: &[&str]) -> Result<String, eyre::Report> {
        const NUM_RETRIES: usize = 18;
        const RETRY_DELAY_SECONDS: u64 = 10;

        let mut url = format!(
            "http://{}/r?json=1&rapi=%24{}",
            self.openevse_hostname, command[0]
        );
        for arg in command[1..].iter() {
            url += &format!("+{arg}");
        }

        for _ in 0..NUM_RETRIES {
            match reqwest::get(&url).await {
                Ok(response) => {
                    match response.text().await {
                        Ok(body) => {
                            let rapi_reply: RapiReply = serde_json::from_str(&body)?;
                            // Some RAPI commands return a string like
                            // "$OK 26400 -1^0C" that we can split on
                            // whitespace, but some return a string like
                            // "$OK^20" that we can not. :-(
                            return Ok(rapi_reply.ret);
                        }
                        Err(e) => {
                            println!("OpenEVSE request text failed: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    println!("OpenEVSE request failed: {:?}", e);
                }
            }
            // If we get here, the request failed and we should sleep
            // a bit then retry (or give up).
            tokio::time::sleep(tokio::time::Duration::from_secs(RETRY_DELAY_SECONDS)).await;
        }
        return Err(eyre::Report::msg(format!(
            "giving up after {} OpenEVSE Request failures",
            NUM_RETRIES
        )));
    }
}
