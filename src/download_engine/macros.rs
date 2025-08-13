/// logs the error and returns Ok to terminate execution without causing an engine restart
#[macro_export]
macro_rules! engine_warn {
    ($msg:expr) => {{
        println!("Engine error: {}", $msg);
        return Ok(());
    }};
    ($fmt:expr, $($arg:tt)+) => {{
        println!("Engine error: {}", format!($fmt, $($arg)+));
        return Ok(());
    }};
}

#[macro_export]
macro_rules! unwrap_or_bail {
    ($opt:expr, $($arg:tt)*) => {
        match $opt {
            Some(val) => val,
            None => anyhow::bail!($($arg)*),
        }
    };
}
