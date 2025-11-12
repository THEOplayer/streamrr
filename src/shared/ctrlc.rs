use tokio::select;
use tokio::signal;
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
    let mut aborted = false;
    let task_handle = task.abort_handle();
    loop {
        select! {
            result = &mut task => return match result {
                Ok(Ok(result)) => Ok(result),
                Ok(Err(err)) => Err(AbortError::Other(err)),
                Err(err) => Err(AbortError::Join(err))
            },
            _ = signal::ctrl_c() => {
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
