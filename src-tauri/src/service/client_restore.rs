use crate::error::AppResult;

pub(super) fn restore(
    restore_config: impl FnOnce() -> AppResult<String>,
    is_registered: impl FnOnce() -> bool,
    running_port: impl FnOnce() -> Option<u16>,
    register: impl FnOnce(u16),
) -> AppResult<String> {
    let message = restore_config()?;
    if is_registered() {
        if let Some(port) = running_port() {
            register(port);
        }
    }
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;
    use std::cell::RefCell;

    #[test]
    fn restores_before_re_registering_and_preserves_result() {
        let calls = RefCell::new(Vec::new());
        let result = restore(
            || {
                calls.borrow_mut().push("restore");
                Ok("restored".into())
            },
            || true,
            || Some(9600),
            |port| {
                assert_eq!(port, 9600);
                calls.borrow_mut().push("register");
            },
        )
        .unwrap();
        assert_eq!(result, "restored");
        assert_eq!(*calls.borrow(), ["restore", "register"]);
    }

    #[test]
    fn failed_restore_does_not_register() {
        let result = restore(
            || Err(AppError::Other("fixture failure".into())),
            || panic!("must stop after failure"),
            || panic!("must not query port"),
            |_| panic!("must not register"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn stopped_mcp_does_not_register() {
        restore(
            || Ok("restored".into()),
            || true,
            || None,
            |_| panic!("must not write client configuration"),
        )
        .unwrap();
    }
}
