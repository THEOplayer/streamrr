use tokio::select;
use tokio::signal;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub async fn abort_on_ctrlc<T, E>(
    mut task: JoinHandle<Result<T, E>>,
    token: CancellationToken,
    error_on_abort: E,
) -> Result<T, E> {
    let mut aborted = false;
    let task_handle = task.abort_handle();
    loop {
        select! {
            result = &mut task => return match result {
                Ok(result) => result,
                Err(_) => Err(error_on_abort)
            },
            _ = signal::ctrl_c() => {
                if !aborted {
                    // First CTRL-C: stop gracefully.
                    aborted = true;
                    token.cancel();
                } else {
                    // Second CTRL-C: force stop.
                    task_handle.abort()
                }
            }
        }
    }
}
