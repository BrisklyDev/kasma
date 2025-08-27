use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct NetworkProxy {
    address: String,
    username: String,
    password: String,
}

#[derive(Clone, Debug)]
pub struct DownloadSetting {
    pub proxy: Option<NetworkProxy>,
    pub progress_polling_frequency_millis: u64,
    pub reset_timeout_millis: u64,
    pub total_connections: u8,
    pub base_save_dir: PathBuf,
    pub base_temp_dir: PathBuf,
    pub logger_enabled: bool,
}

impl DownloadSetting {
    pub fn builder() -> DownloadSettingBuilder {
        DownloadSettingBuilder::default()
    }
}

#[derive(Default)]
pub struct DownloadSettingBuilder {
    proxy: Option<NetworkProxy>,
    progress_polling_milliseconds: Option<u64>,
    total_connections: Option<u8>,
    base_save_dir: Option<PathBuf>,
    base_temp_dir: Option<PathBuf>,
    reset_timeout_millis: Option<u64>,
    logger: Option<bool>,
}

impl DownloadSettingBuilder {
    pub fn proxy(mut self, proxy: NetworkProxy) -> Self {
        self.proxy = Some(proxy);
        self
    }

    pub fn progress_polling_milliseconds(mut self, ms: u64) -> Self {
        self.progress_polling_milliseconds = Some(ms);
        self
    }

    pub fn total_connections(mut self, count: u8) -> Self {
        self.total_connections = Some(count);
        self
    }

    pub fn base_save_dir<P: Into<PathBuf>>(mut self, dir: P) -> Self {
        self.base_save_dir = Some(dir.into());
        self
    }

    pub fn base_temp_dir<P: Into<PathBuf>>(mut self, dir: P) -> Self {
        self.base_temp_dir = Some(dir.into());
        self
    }

    pub fn with_logger(mut self) -> Self {
        self.logger = Some(true);
        self
    }

    pub fn reset_timeout_millis(mut self, ms: u64) -> Self {
        self.reset_timeout_millis = Some(ms);
        self
    }

    pub fn build(self) -> DownloadSetting {
        DownloadSetting {
            proxy: self.proxy,
            progress_polling_frequency_millis: self.progress_polling_milliseconds.unwrap_or(200),
            total_connections: self.total_connections.unwrap_or(8),
            base_save_dir: self.base_save_dir.unwrap_or(PathBuf::from("./downloads")),
            base_temp_dir: self.base_temp_dir.unwrap_or(PathBuf::from("./temp")),
            reset_timeout_millis: self.reset_timeout_millis.unwrap_or(60_000),
            logger_enabled: self.logger.unwrap_or(false),
        }
    }
}
