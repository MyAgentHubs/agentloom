use super::*;

pub(super) fn set_status(
    state: &GatewayInnerState,
    gateway_state: GatewayState,
    last_error: Option<String>,
) {
    *lock(&state.status) = GatewayStatus {
        state: gateway_state,
        last_error,
        stopped_reason: None,
        counters: GatewayCounters::default(),
    };
}

pub(super) fn set_stopped_status(
    state: &GatewayInnerState,
    last_error: String,
    stopped_reason: Option<String>,
) {
    *lock(&state.status) = GatewayStatus {
        state: GatewayState::Disabled,
        last_error: Some(last_error),
        stopped_reason,
        counters: GatewayCounters::default(),
    };
}

pub(super) fn interruptible_sleep(inner: &Inner, duration: Duration) -> bool {
    let mut remaining = duration;
    while !remaining.is_zero() {
        if inner.shutdown.load(Ordering::Acquire) {
            return true;
        }
        if inner.reload_requested.load(Ordering::Acquire) {
            return false;
        }
        if inner.registry_publish_wake.load(Ordering::Acquire) {
            return false;
        }
        let slice = remaining.min(BACKOFF_POLL_INTERVAL);
        thread::sleep(slice);
        remaining = remaining.saturating_sub(slice);
    }
    inner.shutdown.load(Ordering::Acquire)
}

pub(super) fn wait_for_reload(inner: &Inner) -> bool {
    loop {
        if inner.shutdown.load(Ordering::Acquire) {
            return true;
        }
        if inner.reload_requested.load(Ordering::Acquire) {
            return false;
        }
        if inner.registry_publish_wake.load(Ordering::Acquire) {
            return false;
        }
        thread::sleep(BACKOFF_POLL_INTERVAL);
    }
}

pub(super) fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let is_gateway_thread = std::thread::current()
            .name()
            .map(|name| name == "remote-gateway")
            .unwrap_or(false);
        if !is_gateway_thread {
            previous(info);
            return;
        }
        match GATEWAY
            .get()
            .and_then(|inner| inner.active_token.try_lock().ok())
        {
            Some(guard) => {
                let message = any_payload_message(info.payload());
                let redacted = redact_panic_message(&message, guard.as_deref());
                eprintln!("remote gateway panic (redacted): {redacted}");
            }
            None => {
                eprintln!("redacted panic in remote-gateway thread");
            }
        }
    }));
}

pub(super) fn set_active_token(inner: &Inner, token: Option<&SecretToken>) {
    *lock(&inner.active_token) = token.map(|token| token.expose().to_owned());
}

pub(super) fn clear_active_token(inner: &Inner) {
    *lock(&inner.active_token) = None;
}

pub(super) fn take_active_token(inner: &Inner) -> Option<String> {
    lock(&inner.active_token).take()
}

pub(super) fn redact_panic_message(message: &str, active_token: Option<&str>) -> String {
    redact(message, active_token)
}

fn any_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

pub(super) fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    any_payload_message(payload.as_ref())
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
