use crate::engine::workers::download_worker::DownloadWorker;

struct HttpDownloadWorker {
    download_uid: String,
}

impl DownloadWorker for HttpDownloadWorker {
    fn start(&self) {
        todo!()
    }

    fn pause(&self) {
        todo!()
    }

    fn cancel(&self) {
        todo!()
    }
}

