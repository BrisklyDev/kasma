use std::thread;

pub trait DownloadWorker {
    fn spawn_worker_thread(&self) -> thread::JoinHandle<()>;

    fn start(&mut self);

    fn pause(&self);

    fn cancel(&self);
}
