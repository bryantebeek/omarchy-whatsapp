// Small shared helpers used by every local modelling module.

pub(crate) fn nonempty(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.trim().to_owned())
}
