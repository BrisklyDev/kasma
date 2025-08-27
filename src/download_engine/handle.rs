use crate::download_engine::http::http_download_engine::HttpDownloadEngine;
use crate::download_engine::http::message::{DownloadCommand, EngineToMainMsg};
use crate::download_engine::http::progress::DownloadProgress;
use crate::download_engine::setting::DownloadSetting;
use crate::download_engine::{DownloadInfo, RunnableTask};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use tokio::sync::mpsc;
use uuid::Uuid;

pub struct DownloadHandle {
    pub tx: mpsc::Sender<DownloadCommand>,
    pub rx: mpsc::Receiver<EngineToMainMsg>,
    pub join: JoinHandle<()>,
}

impl DownloadHandle {
    pub async fn pause(&self) {
        let _ = self.tx.send(DownloadCommand::Pause).await;
    }

    pub async fn start(&self) {
        let _ = self.tx.send(DownloadCommand::Start).await;
    }

    pub async fn next_event(&mut self) -> Option<EngineToMainMsg> {
        self.rx.recv().await
    }
}

pub fn spawn_http_engine(setting: DownloadSetting, info: DownloadInfo) -> DownloadHandle {
    let (engine_to_main_tx, engine_to_main_rx) = tokio::sync::mpsc::channel::<EngineToMainMsg>(100);
    let (main_to_engine_tx, main_to_engine_rx) = tokio::sync::mpsc::channel::<DownloadCommand>(100);
    let handle = thread::spawn(move || {
        HttpDownloadEngine::new(main_to_engine_rx, engine_to_main_tx, info, setting, None)
            .0
            .run();
    });
    DownloadHandle {
        tx: main_to_engine_tx,
        rx: engine_to_main_rx,
        join: handle,
    }
}
