pub trait DownloadWorker {
    fn start(&self);

    fn pause(&self);

    fn cancel(&self);
}

