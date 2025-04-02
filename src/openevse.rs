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

    pub async fn get_current_capacity(&self) -> Result<f32, eyre::Report> {
        let _data = self.request(&["GE"]).await?;
        // println!("get_current_capacity: {}", data);
        Ok(0.0)
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
        let mut url = format!(
            "http://{}/r?json=1&rapi=%24{}",
            self.openevse_hostname, command[0]
        );
        for arg in command[1..].iter() {
            url += &format!("+{arg}");
        }

        let body = reqwest::get(url).await?.text().await?;

        let rapi_reply: RapiReply = serde_json::from_str(&body)?;

        // Some RAPI commands return a string like "$OK 26400 -1^0C"
        // that we can split on whitespace, but some return a string like
        // "$OK^20" that we can not. :-(

        Ok(rapi_reply.ret)
    }
}
