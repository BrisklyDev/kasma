use tokio::time::Duration;
use tokio::time::interval;

pub async fn spawn_timer<F>(duration: Duration, callback: F) {
    let mut ticker = interval(duration);
    loop {
        ticker.tick().await;
    }

    let handle = tokio::spawn(async {
        let mut ticker = interval(Duration::from_secs(2));
        loop {
            ticker.tick().await;
            println!("Tick");
        }
    });
}
