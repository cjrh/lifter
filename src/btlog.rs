use backtrace::Backtrace;
use log::error;

pub fn log_error_with_stack_trace<T: AsRef<str>>(message: T) {
    // Don't include this function in the Backtrace
    // because it's not useful
    let bt = Backtrace::new();
    error!("{}: {:?}", message.as_ref(), bt);
}
