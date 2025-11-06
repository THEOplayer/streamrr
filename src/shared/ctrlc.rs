use tokio::select;
use tokio::sync::mpsc::{Receiver, channel};
use tokio::task::{JoinError, JoinHandle};

#[derive(thiserror::Error, Debug)]
pub enum AbortError<E> {
    #[error("io error: {0}")]
    Io(std::io::Error),
    #[error("cancelled")]
    Join(JoinError),
    #[error(transparent)]
    Other(E),
}

pub async fn abort_on_ctrlc<T, E>(mut task: JoinHandle<Result<T, E>>) -> Result<T, AbortError<E>> {
    let mut ctrlc_stream = ctrlc_channel().map_err(AbortError::Io)?;
    let mut aborted = false;
    let task_handle = task.abort_handle();
    loop {
        select! {
            result = &mut task => return match result {
                Ok(Ok(result)) => Ok(result),
                Ok(Err(err)) => Err(AbortError::Other(err)),
                Err(err) => Err(AbortError::Join(err))
            },
            _ = ctrlc_stream.recv() => {
                if !aborted {
                    // First CTRL-C: stop gracefully.
                    aborted = true;
                    task_handle.abort();
                } else {
                    // Second CTRL-C: force stop.
                    std::process::exit(1);
                }
            }
        }
    }
}

fn ctrlc_channel() -> std::io::Result<Receiver<()>> {
    // https://rust-cli.github.io/book/in-depth/signals.html#using-channels
    let (sender, receiver) = channel(16);
    ctrlc::set_handler(move || {
        sender.try_send(()).unwrap();
    })
    .map_err(|e| match e {
        ctrlc::Error::System(e) => e,
        ctrlc_error => std::io::Error::other(ctrlc_error),
    })?;
    Ok(receiver)
}
