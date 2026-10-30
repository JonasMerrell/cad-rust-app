//! crackh3d's `repro` hash tracing, off (the solver calls it when `CRACKH3D_TRACE_HASH` is set).

pub fn trace_enabled() -> bool {
    false
}

pub fn trace_hash(_label: &str, _vals: impl IntoIterator<Item = f64>) {}
