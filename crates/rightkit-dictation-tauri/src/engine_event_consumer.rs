use std::sync::{mpsc, Arc, Mutex};

/// Spawn a consumer without moving receiver ownership into a closure until
/// the OS accepts the thread. A failed spawn returns receiver for retry.
pub fn spawn_receiver_consumer<T, F, S>(
    receiver: mpsc::Receiver<T>,
    consume: F,
    spawn: S,
) -> Result<(), (String, mpsc::Receiver<T>)>
where
    T: Send + 'static,
    F: FnOnce(mpsc::Receiver<T>) + Send + 'static,
    S: FnOnce(Box<dyn FnOnce() + Send>) -> Result<(), String>,
{
    let holder = Arc::new(Mutex::new(Some(receiver)));
    let thread_holder = Arc::clone(&holder);
    let job = Box::new(move || {
        let receiver = thread_holder
            .lock()
            .ok()
            .and_then(|mut receiver| receiver.take());
        if let Some(receiver) = receiver {
            consume(receiver);
        }
    });
    match spawn(job) {
        Ok(()) => Ok(()),
        Err(error) => {
            let receiver = holder
                .lock()
                .ok()
                .and_then(|mut receiver| receiver.take())
                .expect("failed thread spawn must retain event receiver");
            Err((error, receiver))
        }
    }
}
