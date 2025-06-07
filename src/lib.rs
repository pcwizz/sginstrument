unsafe extern "C" {
    fn __sfuzzer_instrument(location: std::os::raw::c_uint, state_value: std::os::raw::c_uint);
}

/// Informs the fuzzer that a new state has been reached.
pub fn instrument(location: u32, state_value: u32) {
    unsafe {
        __sfuzzer_instrument(location, state_value);
    }
}

#[cfg(test)]
mod test {
    use crate::instrument;

    #[test]
    fn test() {
        instrument(0, 0);
    }
}
